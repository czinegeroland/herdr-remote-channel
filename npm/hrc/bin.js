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

import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import { dirname, join, sep } from 'node:path'

const require = createRequire(import.meta.url)
const binary = process.platform === 'win32' ? 'hrc.exe' : 'hrc'

// Keep in step with `scripts/build-npm-packages.mjs`; a test holds them
// together.
const PACKAGES = {
  'linux x64': 'linux-x64',
  'darwin x64': 'darwin-x64',
  'darwin arm64': 'darwin-arm64',
  'win32 x64': 'win32-x64',
}

// A source checkout linked with `herdr plugin link` runs its own build.
//
// The manifest starts every action as `node node_modules/herdr-remote-channel/bin.js`,
// and `plugin link` runs no build step, so a checkout has no such package.
// The README's development flow links that path to `npm/hrc`, the source of
// this file. Node resolves a symlinked main module to its real path, so this
// file then runs as `npm/hrc/bin.js` inside the checkout -- which an npm
// install never does, because there it is a copy at the package root. That
// is the whole test: no environment variable, and nothing an installed
// plugin can fall into by accident. The executable is the one
// `cargo install --path crates/hrc-cli --root .` writes to `bin/`.
const here = dirname(fileURLToPath(import.meta.url))

if (here.endsWith(`${sep}npm${sep}hrc`)) {
  const local = join(here, '..', '..', 'bin', binary)

  if (!existsSync(local)) {
    console.error(
      `hrc: this is a source checkout, but ${local} has not been built.\n` +
        `Build it with: cargo install --path crates/hrc-cli --root . --locked --force`,
    )
    process.exit(1)
  }

  run(local)
} else {
  run(published())
}

// The prebuilt executable npm installed for this platform.
function published() {
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

  const pkg = `herdr-remote-channel-${suffix}`

  try {
    return require.resolve(`${pkg}/${binary}`)
  } catch {
    // The optional dependency did not install. The usual cause is
    // `--no-optional`, or a lockfile made on a different platform.
    console.error(
      `hrc: ${pkg} is not installed, so there is no binary for ${key}.\n` +
        `If you installed with --no-optional, reinstall without it.`,
    )
    process.exit(1)
  }
}

// Runs the executable in the foreground and exits the way it did.
function run(resolved) {
  // `stdio: inherit` so the CLI owns the terminal: `hrc review` drives a
  // trusted full-screen interface, and a wrapper that buffered its output
  // would break it.
  //
  // Asynchronous rather than `spawnSync`, because this process has to stay
  // responsive to signals. `spawnSync` blocks the event loop for the whole run,
  // so a handler registered here could never fire, and killing this shim left
  // the real executable running as an orphan: `kill <pid of hrc>` reaped node
  // and the daemon underneath it carried on holding its socket. That is the
  // same shape as the Windows orphan that withdrew daemon autostart in
  // decision DEC-068, and it is why the end-to-end run reported
  // `Terminate orphan process: (hrc)` on a green job.
  const child = spawn(resolved, process.argv.slice(2), { stdio: 'inherit' })

  child.on('error', (error) => {
    console.error(`hrc: could not run ${resolved}: ${error.message}`)
    process.exit(1)
  })

  // Forward what a supervisor actually sends. The child gets the signal and
  // decides what it means -- the daemon, for one, shuts down cleanly on
  // SIGTERM -- and this process then exits by the child's own outcome below,
  // so the exit code still describes the executable rather than the wrapper.
  //
  // SIGINT from a terminal already reaches the child through the foreground
  // process group, so forwarding it is belt and braces; a `kill` aimed at this
  // pid is the case that was broken.
  for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) {
    process.on(signal, () => {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill(signal)
      }
    })
  }

  // Signals do not survive as exit codes. Report them the way a shell does, so
  // a daemon stopped with SIGTERM is not mistaken for a clean exit.
  child.on('exit', (status, signal) => {
    process.exit(status === null ? 128 + (signal === 'SIGINT' ? 2 : 15) : status)
  })
}
