#!/usr/bin/env node
// Resolves the prebuilt `hrc` for this platform and runs it.
//
// The binary is not downloaded at install time. Each platform's executable is
// published inside its own npm package, and this package depends on all of
// them through `optionalDependencies` with `os` and `cpu` constraints, so npm
// installs exactly the one that can run here and skips the rest.
//
// That is deliberate. The usual shape for a Rust CLI on npm is a postinstall
// script that fetches a release archive over HTTPS and trusts it. That means
// an unverified download on every install, a hard dependency on the release
// host staying reachable, and nothing at all if the repository is private.
// Bundling instead means npm serves the bytes under its own integrity hash,
// and `scripts/build-npm-packages.mjs` has already checked each executable
// against its published SHA-256 before it was ever packed.

import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)

// Keep in step with `scripts/build-npm-packages.mjs`; a test holds them
// together.
const PACKAGES = {
  'linux x64': 'linux-x64',
  'darwin x64': 'darwin-x64',
  'darwin arm64': 'darwin-arm64',
  'win32 x64': 'win32-x64',
}

const key = `${process.platform} ${process.arch}`
const suffix = PACKAGES[key]

if (!suffix) {
  console.error(
    `hrc: no prebuilt binary for ${key}.\n` +
      `Supported: ${Object.keys(PACKAGES).join(', ')}.\n` +
      `Build from source instead: cargo install --path crates/hrc-cli`,
  )
  process.exit(1)
}

const pkg = `@herdr-remote-channel/${suffix}`
const binary = process.platform === 'win32' ? 'hrc.exe' : 'hrc'

let resolved
try {
  resolved = require.resolve(`${pkg}/${binary}`)
} catch {
  // The optional dependency did not install. The usual cause is
  // `--no-optional`, or a lockfile made on a different platform.
  console.error(
    `hrc: ${pkg} is not installed, so there is no binary for ${key}.\n` +
      `If you installed with --no-optional, reinstall without it.`,
  )
  process.exit(1)
}

// `stdio: inherit` so the CLI owns the terminal: `hrc review` drives a
// trusted full-screen interface, and a wrapper that buffered its output
// would break it.
const result = spawnSync(resolved, process.argv.slice(2), { stdio: 'inherit' })

if (result.error) {
  console.error(`hrc: could not run ${resolved}: ${result.error.message}`)
  process.exit(1)
}

// Signals do not survive as exit codes. Report them the way a shell does, so
// a daemon stopped with SIGTERM is not mistaken for a clean exit.
process.exit(result.status === null ? 128 + (result.signal ? 15 : 0) : result.status)
