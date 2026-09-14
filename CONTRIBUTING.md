# Contributing

Thanks for looking. Two things are unusual about this repository, and both
will reject your pull request if you do not know them first.

## 1. `docs/PRD.md` is the specification, and it is living

`docs/PRD.md` is the authoritative product and delivery specification. It is
not documentation written after the fact; behaviour is defined there first.

**Every pull request must update it**, including:

- the requirement rows whose status or evidence your change actually alters,
- the section 31 delivery ledger,
- the `Last updated` timestamp.

Your pull request description must then name the requirement IDs you touched,
or give a concrete no-progress rationale in the form
`No requirement progress: <reason>`.

The `PRD traceability` check enforces this and will fail the pull request
otherwise. A common way to trip it is naming a requirement ID in the
description whose row you did not in fact change — say what you changed, not
what you are near.

`.github/pull_request_template.md` has the expected structure.

## 2. The human authorization boundary is not negotiable

The operations in PRD section 22.7 must stay impossible for agent-safe and
non-interactive callers: approving a join, removing a member, revoking a
device, granting a capability, making a repository public, and reading or
approving pending message content.

A change that makes any of these reachable without a human at the trusted
interface will not be merged, however convenient it is. If you believe one of
them is wrong, argue that in an issue and change the PRD first.

The same goes for the prompt gate: inbound content is quarantined until a
local human approves it, and no agent-accessible surface may return a pending
body. `crates/hrc-core/src/rpc.rs` enforces this in the type system rather
than in handlers, deliberately — there is no response variant that can carry
an unapproved body. Please keep it that way.

## Before you push

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
```

Pull-request CI runs these on Linux, Windows and macOS, and skips them
entirely when no Rust source changed, so you will hear about a
platform-specific problem on the pull request rather than after it merges.

`docs/PRD.md` has CRLF line endings. Edit it in a way that preserves them.

## Style

Match the code around you. Comments here explain *why* a thing is the way it
is — particularly where the safe option was chosen over the obvious one —
rather than restating what the line does. Tests are named as sentences
describing the property they hold.

## Security

Do not open a public issue for a security problem. See
[SECURITY.md](SECURITY.md).
