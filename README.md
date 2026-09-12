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

## Repository layout

```text
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

CI runs the same commands on Linux, macOS, and Windows.

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

## Contributing

Every pull request must update `docs/PRD.md`, including the delivery ledger
and the `Last updated` timestamp, and must name the requirement IDs it
touches or give a concrete no-progress rationale. The `PRD traceability`
check enforces this. `.github/pull_request_template.md` has the expected
structure.

