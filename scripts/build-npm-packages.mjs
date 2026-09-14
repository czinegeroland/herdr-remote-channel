#!/usr/bin/env node
// Turns the release archives into npm packages, verifying them on the way.
//
//   node scripts/build-npm-packages.mjs --version 0.1.0 \
//     --artifacts <dir of release archives and .sha256 files> \
//     --out <dir to write packages into>
//
// Produces one package per platform, each carrying that platform's `hrc`
// executable, plus the `herdr-remote-channel` package whose `bin` resolves
// among them at run time.
//
// The verification here is the point. `cargo dist` can generate an npm
// installer, but it fetches the archive over HTTPS at install time and checks
// nothing — this project publishes SHA-256 checksums precisely so that an
// artifact can be refused, and a distribution channel that ignores them
// discards that. So every archive is checked against its published `.sha256`
// before it is unpacked, and a mismatch aborts the whole build rather than
// producing a package that is merely missing one platform.

import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { mkdirSync, readFileSync, writeFileSync, copyFileSync, rmSync, existsSync } from 'node:fs'
import { basename, join } from 'node:path'

const SCOPE = '@herdr-remote-channel'
const ROOT_PACKAGE = 'herdr-remote-channel'

// The four targets PRD section 13.2 requires, mapped to what npm calls them.
const TARGETS = [
  { target: 'x86_64-unknown-linux-gnu', suffix: 'linux-x64', os: 'linux', cpu: 'x64', ext: '.tar.xz', binary: 'hrc' },
  { target: 'x86_64-apple-darwin', suffix: 'darwin-x64', os: 'darwin', cpu: 'x64', ext: '.tar.xz', binary: 'hrc' },
  { target: 'aarch64-apple-darwin', suffix: 'darwin-arm64', os: 'darwin', cpu: 'arm64', ext: '.tar.xz', binary: 'hrc' },
  { target: 'x86_64-pc-windows-msvc', suffix: 'win32-x64', os: 'win32', cpu: 'x64', ext: '.zip', binary: 'hrc.exe' },
]

function arg(name, required = true) {
  const index = process.argv.indexOf(`--${name}`)
  if (index === -1 || !process.argv[index + 1]) {
    if (!required) return undefined
    throw new Error(`missing --${name}`)
  }
  return process.argv[index + 1]
}

const version = arg('version').replace(/^v/, '')
const artifacts = arg('artifacts')
const out = arg('out')

if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  throw new Error(`--version must be a semantic version, got "${version}"`)
}

rmSync(out, { recursive: true, force: true })
mkdirSync(out, { recursive: true })

/**
 * Refuses an archive whose digest does not match the published one.
 *
 * Throws rather than warning. A checksum that can be skipped by accident is
 * not a checksum, which is the same reasoning `scripts/install.sh` is built
 * on.
 */
function verify(archive) {
  const checksumFile = `${archive}.sha256`
  if (!existsSync(checksumFile)) {
    throw new Error(`${basename(archive)} has no published checksum; refusing to package it`)
  }

  // The file is `<digest>  <name>`, as `sha256sum` writes it.
  const published = readFileSync(checksumFile, 'utf8').trim().split(/\s+/)[0].toLowerCase()
  const actual = createHash('sha256').update(readFileSync(archive)).digest('hex')

  if (actual !== published) {
    throw new Error(
      `checksum mismatch for ${basename(archive)}:\n` +
        `  published ${published}\n` +
        `  actual    ${actual}\n` +
        `Refusing to install.`,
    )
  }
  return actual
}

const optionalDependencies = {}
const verified = []

for (const platform of TARGETS) {
  const archive = join(artifacts, `hrc-cli-${platform.target}${platform.ext}`)
  if (!existsSync(archive)) {
    throw new Error(`${basename(archive)} is missing; every supported platform must be published together`)
  }

  const digest = verify(archive)
  verified.push({ archive: basename(archive), digest })

  const dir = join(out, platform.suffix)
  mkdirSync(dir, { recursive: true })

  // Unpacked with the platform's own tool rather than a bundled extractor, so
  // there is no third-party archive parser in the publishing path.
  const staging = join(out, `.unpack-${platform.suffix}`)
  mkdirSync(staging, { recursive: true })
  if (platform.ext === '.zip') {
    execFileSync('unzip', ['-q', '-o', archive, '-d', staging])
  } else {
    execFileSync('tar', ['-xf', archive, '-C', staging])
  }

  const found = execFileSync('find', [staging, '-name', platform.binary, '-type', 'f'])
    .toString()
    .trim()
    .split('\n')
    .filter(Boolean)
  if (found.length !== 1) {
    throw new Error(`expected exactly one ${platform.binary} in ${basename(archive)}, found ${found.length}`)
  }
  copyFileSync(found[0], join(dir, platform.binary))
  rmSync(staging, { recursive: true, force: true })

  const name = `${SCOPE}/${platform.suffix}`
  writeFileSync(
    join(dir, 'package.json'),
    `${JSON.stringify(
      {
        name,
        version,
        description: `The hrc executable for ${platform.os} ${platform.cpu}.`,
        license: 'Apache-2.0',
        repository: { type: 'git', url: 'git+https://github.com/czinegeroland/herdr-remote-channel.git' },
        // npm reads these to decide which optional dependency to install, so
        // a machine only ever downloads the binary it can run.
        os: [platform.os],
        cpu: [platform.cpu],
        files: [platform.binary],
      },
      null,
      2,
    )}\n`,
  )

  optionalDependencies[name] = version
}

// The package a person installs.
const rootDir = join(out, ROOT_PACKAGE)
mkdirSync(rootDir, { recursive: true })
copyFileSync('npm/hrc/bin.js', join(rootDir, 'bin.js'))
copyFileSync('LICENSE', join(rootDir, 'LICENSE'))
copyFileSync('README.md', join(rootDir, 'README.md'))
writeFileSync(
  join(rootDir, 'package.json'),
  `${JSON.stringify(
    {
      name: ROOT_PACKAGE,
      version,
      description: 'Secure asynchronous communication between independent Herdr installations.',
      license: 'Apache-2.0',
      repository: { type: 'git', url: 'git+https://github.com/czinegeroland/herdr-remote-channel.git' },
      homepage: 'https://github.com/czinegeroland/herdr-remote-channel',
      keywords: ['herdr', 'herdr-plugin', 'cli', 'end-to-end-encryption'],
      type: 'module',
      bin: { hrc: 'bin.js' },
      files: ['bin.js', 'LICENSE', 'README.md'],
      // No `postinstall`. There is nothing to fetch and nothing to run: npm
      // resolves the one platform package that matches and the shim execs the
      // binary out of it.
      optionalDependencies,
      engines: { node: '>=18' },
    },
    null,
    2,
  )}\n`,
)

console.log(`Built ${TARGETS.length + 1} packages for ${version} in ${out}`)
for (const entry of verified) {
  console.log(`  verified ${entry.archive} ${entry.digest.slice(0, 16)}…`)
}
