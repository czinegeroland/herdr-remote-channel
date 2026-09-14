# Herdr Remote Channel

Secure, asynchronous communication between independent Herdr sessions
running on different machines and owned by different people.

`hrc` lets two Herdr users and their local coding agents exchange notes,
questions, answers, delegation requests, and bounded context packages. All
content is end-to-end encrypted before it reaches the transport, every
participant and device holds its own keys, and inbound content is quarantined
until a local human approves it. It is a communication layer, not remote
control: there is no remote shell, no workspace synchronization, and no
automatic delivery of remote content into an agent prompt.

`docs/PRD.md` is the authoritative product and delivery specification. Read it
before changing behavior, and update it in the same pull request.

## Status

Milestone M1 (secure channel foundation) is in progress. The published `hrc`
command surface exists and the human authorization boundary is enforced;
channel, messaging, and synchronization behavior land with their milestones.
Section 30 of the PRD tracks the roadmap and section 31 the delivery ledger.

## Installing

The Herdr plugin and the `hrc` CLI are the same executable, so one install
gets you both.

### As a Herdr plugin

```bash
herdr plugin install czinegeroland/herdr-remote-channel
```

Herdr clones the repository, shows you the commands it is about to run, and
on your confirmation runs the manifest's build steps. They are three `cargo`
invocations and nothing else — the same three on Linux, macOS and Windows:

1. `cargo install --path crates/hrc-cli --root .` puts the binary at
   `bin/hrc` inside the plugin directory, which is what `herdr-plugin.toml`
   invokes. Resolving it relative to the plugin root rather than through
   `PATH` means the plugin works whether or not the install prefix is on the
   `PATH` Herdr inherited. Cargo appends `.exe` on Windows itself.
2. `cargo install --path crates/hrc-cli` puts a second copy in Cargo's own
   bin directory, so `hrc send`, `hrc doctor` and the agent skill's
   `hrc --help` discovery work in your terminal. Rustup already puts that
   directory on your `PATH`.
3. `cargo clean` reclaims the build directory the first two filled — about
   400 MB, which would otherwise sit in the Herdr plugins folder for as long
   as the plugin is installed. The cost is that a reinstall rebuilds.

This needs a Rust toolchain ([rustup](https://rustup.rs)); the first build
takes a couple of minutes. There is deliberately no shell script on either
side: an earlier version had one per platform, and the PowerShell half failed
on the first real Windows install while the POSIX half worked, because two
scripts are two implementations of one idea and only one of them had been
run.

Confirm the install:

```bash
herdr plugin list
hrc --version
hrc doctor
```

### For local development

```bash
git clone https://github.com/czinegeroland/herdr-remote-channel
cd herdr-remote-channel
cargo install --path crates/hrc-cli --root . --locked --force
herdr plugin link "$PWD"
```

`herdr plugin link` does not run build commands, which is why the install is
invoked by hand first. Re-run it after any change to the Rust sources. Skip
the `cargo clean` step here — you want the build cache.

### The CLI on its own, with no toolchain

Once a release is tagged, the CLI is on npm and needs no Rust:

```bash
npx herdr-remote-channel@latest --version
```

The binary is not downloaded when you install. Each platform's executable is
published inside its own npm package and selected by npm's `os` and `cpu`
constraints, so npm serves the bytes under its own integrity hash and nothing
is fetched from GitHub at install time. Every archive is checked against its
published SHA-256 before it is packed, and a mismatch stops the publish.

There is also a shell installer, which does the same verification itself:

```bash
curl -fsSL https://raw.githubusercontent.com/czinegeroland/herdr-remote-channel/main/scripts/install.sh | sh -s -- --version v0.1.0
```

`scripts/install.ps1` is the Windows equivalent. Both download a prebuilt
binary, verify its SHA-256 against the published checksum, and refuse an
artifact that does not match. Neither compiles anything.

## What the plugin registers

`herdr-plugin.toml` is generated from `crates/hrc-herdr/src/manifest.rs` and
held to it by `crates/hrc-cli/tests/plugin_manifest.rs`, so the entry points
it advertises and the subcommands the binary accepts cannot drift apart. Do
not edit it by hand; regenerate it:

```bash
cargo run --quiet --bin hrc -- herdr manifest > herdr-plugin.toml
```

| Kind | What it does |
|---|---|
| Startup | `hrc herdr startup` — returns the manifest and the sidebar line |
| Action `inbox` | `hrc herdr action inbox` — the remote channel inbox, in workspace and pane contexts |
| Pane `inbox` | `hrc herdr pane inbox` — the same inbox as a split pane |
| Event `workspace.focused` | `hrc herdr event` — refreshes the sidebar |

The event subscription is deliberately one entry long. PRD section 13.2 puts
frequently occurring event processing in the daemon rather than in repeatedly
spawned hook commands, and every name added there is a process spawn at the
host's rate rather than ours. Herdr names the event in `HERDR_PLUGIN_EVENT`;
an event this plugin did not subscribe to is ignored rather than failed, so a
host that delivers one does not show you a broken plugin.

Nothing the plugin can be asked to do approves, delivers, or reveals remote
content. Approval is a human act on the trusted screen (PRD section 19.2),
and the reaction type has no variant that could perform one.

## Repository layout

```text
herdr-plugin.toml     Generated Herdr plugin manifest
crates/
  hrc-protocol/       Wire protocol constants, domains, and message kinds
  hrc-crypto/         Signing, encryption, hashing, invite proofs
  hrc-storage/        SQLite inbox, outbox, quarantine, audit, cursors
  hrc-transport/      Transport adapter contract and conformance suite
  hrc-transport-git/  Built-in Git transport
  hrc-core/           Identity, membership, messaging, prompt gate
  hrc-herdr/          Herdr plugin integration
  hrc-tui/            Trusted inbox and approval interface
  hrc-cli/            The single `hrc` executable
docs/PRD.md           Living product requirements document
```

One Cargo workspace produces one self-contained executable. `hrc daemon`,
`hrc herdr startup`, `hrc herdr action`, `hrc herdr event`, and
`hrc herdr pane` are subcommands of that same binary.

## Building

Requires a stable Rust toolchain with edition 2024 support (1.85 or newer);
`rust-toolchain.toml` selects it automatically.

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

Pull-request CI runs these on Linux, Windows and macOS, and skips them
entirely when no Rust source changed. Formatting and lint run on Linux alone,
because `cargo fmt` and `clippy` give the same answer everywhere.

## Exit codes

Every non-interactive command reports a documented exit code.

| Code | Meaning |
|---:|---|
| 0 | Success |
| 1 | Runtime failure |
| 2 | Invalid command line, or an option the command does not support |
| 3 | Command is published but not implemented yet |
| 4 | The operation requires the trusted local human interface |

## Human authorization boundary

These operations always fail with code 4 for agent-safe and non-interactive
callers, and must be completed in the trusted local interface (PRD section
22.7):

- Reading a pending inbound body, and approving, editing, declining, or
  delivering pending content
- Approving or rejecting a join request
- Removing a member or revoking a device
- Granting capabilities
- Making a channel repository public
- Repository rollover

`hrc review` and `hrc approve` additionally reject `--json`; they never write
pending content to standard output.

## Security

This project moves other people's private conversations, so a defect can
expose one. **Do not open a public issue for a security problem** — report it
privately through
[GitHub's private vulnerability reporting](https://github.com/czinegeroland/herdr-remote-channel/security/advisories/new).
[SECURITY.md](SECURITY.md) sets out what is in scope and what is not.

## License

Apache License 2.0. See [LICENSE](LICENSE).

## Contributing

Every pull request must update `docs/PRD.md`, including the delivery ledger
and the `Last updated` timestamp, and must name the requirement IDs it
touches or give a concrete no-progress rationale. The `PRD traceability`
check enforces this. `.github/pull_request_template.md` has the expected
structure, and [CONTRIBUTING.md](CONTRIBUTING.md) explains both this rule and
the human authorization boundary before you spend effort on a change that
would be rejected.

