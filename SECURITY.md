# Security policy

Herdr Remote Channel carries end-to-end encrypted messages between
installations owned by different people. A defect here is not only a bug in a
program; it can expose a private conversation, or let content reach an agent
that no human approved. Please treat it accordingly.

## Reporting a vulnerability

**Do not open a public issue for a security problem.**

Report privately through GitHub's
[private vulnerability reporting](https://github.com/czinegeroland/herdr-remote-channel/security/advisories/new),
which opens a draft advisory visible only to you and the maintainer.

Please include what you need to make the problem concrete: the version or
commit, the platform, what you did, what happened, and what you expected. A
proof of concept helps and is welcome; so is a report without one.

You should get an acknowledgement within a week. If you do not, assume the
message went astray rather than that it was ignored, and send it again.

## What is in scope

The properties this project is meant to hold, and therefore the ones worth
attacking, are set out in `docs/PRD.md` sections 14 through 22 and 25. In
short:

- **Message confidentiality and integrity.** Content is encrypted to
  recipient devices before it reaches the transport. Anyone who can read the
  Git remote — including the hosting provider — should learn nothing beyond
  object sizes and timing.
- **The prompt gate.** Inbound content is quarantined until a local human
  approves it. An agent-accessible interface that can read a pending body, or
  any path that delivers content to an agent without a one-use,
  digest-bound authorization, is a vulnerability.
- **The human authorization boundary.** The operations in PRD section 22.7 —
  approving a join, removing a member, revoking a device, granting a
  capability, making a repository public, reading pending content — must be
  impossible for a non-interactive or agent-safe caller.
- **Key containment.** Private keys must not be serializable, printable, or
  transmissible, and must be encrypted at rest.
- **Membership.** Forged or replayed membership changes, and roster history
  that does not descend from what was already observed, must be refused.

Reports demonstrating a break in any of these are the most valuable kind.

## What is out of scope

- An attacker who already controls the user's machine or user account. HRC
  protects a conversation between machines; it is not a defence against
  someone who is already running as you.
- Denial of service against a Git host, or the cost of storing objects there.
- Metadata the transport inherently exposes: that a repository exists, its
  object sizes, and when they changed. PRD section 20 is explicit that
  size-and-timing analysis is not defended against.
- `HRC_PASSPHRASE` being readable by other processes running as the same
  user. It is documented as an explicit opt-in for automation rather than the
  recommended path for a person at a terminal.
- Missing hardening in a dependency with no demonstrated impact here.

## Supported versions

This project has not cut its first release. Until it does, the supported
version is the current `main`.

## Disclosure

Please give a reasonable window to ship a fix before publishing. If a report
turns out to describe intended behaviour, expect the reasoning rather than a
dismissal — and if the reasoning is wrong, saying so is itself a useful
contribution.
