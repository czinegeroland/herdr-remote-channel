# Handoff

Written for the next agent to continue this work autonomously. The PRD in
`docs/PRD.md` is the specification; this file is how the work has been run and
where it stands.

Read this once, then work from the PRD.

---

## 1. The goal

Take `docs/PRD.md` from partially implemented to done: every requirement row
in section 12 and section 25 reaching **Implemented** with real evidence, and
every acceptance row in section 30.1 carrying evidence rather than `Pending`.

The PRD is a *living* document. It is not a fixed spec to build against and
leave alone — it is updated in the same pull request as the code that changes
what it describes. CI enforces this; see section 3.

Two things cannot be finished without a human, and should be left flagged
rather than faked:

- M0 carries a product-owner approval gate on the transport adapter
  specification (`AC-ADAPTER-SPEC`).
- The PRD's product and technical owners are `TBD`.

---

## 2. How the loop runs

Autonomously, one pull request at a time. No pausing to ask what to do next;
the PRD says what is missing and the milestone percentages say what is most
behind.

The cycle for each unit of work:

1. **Re-base.** `git fetch origin main && git checkout -B claude/happy-johnson-4kqomk origin/main`.
   Do this immediately after every merge. Building on pre-squash history
   causes conflicts that are tedious to unpick.
2. **Pick the next thing** from the PRD's unimplemented rows, preferring
   whatever unblocks the most other rows.
3. **Implement it**, with tests that would fail if the behaviour were wrong.
4. **Verify locally** before pushing:
   ```
   cargo fmt --all
   cargo clippy --workspace --all-targets --locked   # -D warnings is set in CI
   cargo test --workspace --locked
   ```
   Also `cargo check -p <crate> --all-targets --target x86_64-pc-windows-msvc`
   for crates without a C dependency — see section 6.
5. **Update the PRD** in the same commit (section 3).
6. **Commit**, force-with-lease push to the development branch, open a PR.
7. **Wait for all five checks**, then squash merge.
8. Back to step 1.

### Branch

Develop on `claude/happy-johnson-4kqomk`. Never push to `main` directly.
Force-with-lease pushes to the development branch are normal, because the
branch is re-based onto `main` after each merge.

### Pull requests

Squash merge, always. The PR title becomes the commit subject, so write it as
one.

PR bodies explain *why*, not *what* — the diff already says what. The ones
that have been useful state the problem, the decision taken, and what was
rejected. Where a test caught a real defect, say so plainly: that is the most
valuable sentence in the description.

Every commit message and PR body ends with the attribution trailers already
present on this branch's history — copy the form from `git log`.

### Autonomy boundaries

Merging your own PRs once CI is green is expected here. What is not: pushing
to a branch other than the development branch, deleting or force-pushing
someone else's work, or marking a PRD row Implemented without evidence that
would survive someone checking it.

---

## 3. The living-PRD rule, and its CI check

Every pull request must change `docs/PRD.md`. The validator
(`.github/scripts/check-prd-traceability.ps1`, run under `pull_request_target`
from the trusted base revision) enforces:

- the PRD changed at all;
- the section 31 **delivery ledger** changed — not just section 30.1, they are
  different tables and this catches people out;
- the `Last updated` timestamp moved;
- every `HRC-...` requirement ID named in the PR body has a correspondingly
  changed row in the PRD;
- or, for a PR that makes no requirement progress, a concrete no-progress
  rationale in the body.

This check has been right every time it fired — four times in this session,
each time because a requirement was named in prose whose row had not actually
changed. Fix the reference or change the row; do not work around the
validator.

### Editing `docs/PRD.md`

**The file is CRLF.** A text-mode edit will rewrite all 2,500 lines and bury
the real change. Edit in binary:

```python
raw = path.read_bytes()
assert b"\r\n" in raw
text = raw.decode("utf-8").replace("\r\n", "\n")
# ... replacements, asserting each old string appears exactly once ...
path.write_bytes(text.replace("\n", "\r\n").encode("utf-8"))
```

Assert the count before replacing. Several evidence strings appear in more
than one row, and a blind replace silently edits the wrong requirement.

### What goes in the PRD

- Requirement rows: status and an evidence reference naming files.
- Section 31 ledger: milestone percentage, what the PR added, what is next.
- Section 30.1: acceptance evidence, listing the adversarial cases covered,
  not just "tested".
- Decisions table: a `DEC-0xx` row for any choice a reader would otherwise
  question, with the reasoning and what it rules out.
- Open questions: an `OQ-0xx` row for anything genuinely undecided.

Decisions currently run to DEC-047 and open questions to OQ-013.

---

## 4. Where we left off

*This section is the one with a short shelf life. Update it in the same pull
request as the work it describes, or it will lie.*

### State

**80 of 94 requirement rows Implemented.** Milestones: **M0 55%, M1 100%,
M2 99%, M3 95%, M4 85%, M5 0%.**

`main` is at the Herdr plugin entry points; the development branch is level
with it. Nothing is in flight.

What works today, end to end and proven by tests against a real Git
repository: two installations create a channel, invite, join, derive and
compare matching safety phrases, admit a member through the trusted
interface, exchange encrypted notes and threaded questions and answers, draft
and send source-derived context packages behind a one-use trusted
authorization, and have everything arrive quarantined behind the prompt gate.
Observed history rewrites, deletions, and substitutions halt synchronization
stickily. **And the Herdr plugin now runs**: `hrc herdr startup` returns the
manifest and the sidebar line, `hrc herdr pane inbox` renders the inbox,
and `hrc herdr event` reads one JSON event from standard input.

### Most recent implementation

Message expiry, completing `HRC-MSG-005`.

- `hrc send --expires` and `hrc ask --expires` take a lifetime such as `24h`
  and resolve it to an absolute RFC 3339 timestamp in the signed envelope,
  for the same reason an invite's is resolved: a signed envelope carrying
  "24h" would lapse at a moment that depends on when someone read it.
- `Database::sweep_expired` runs at the start of every `sync --once` and
  every daemon tick. A lapsed quarantined row moves to the `expired`
  disposition and its plaintext is dropped; the row itself stays, because
  section 16.3 says expiration is logical and section 26 says an expired
  message is still displayed. The object is still in Git if it is ever
  needed, since that history is immutable.
- Only `quarantined` rows are swept. A decided message does not lapse; a
  message the human chose to *keep in the inbox* is still quarantined and so
  still does. That second case is the one most likely to be got wrong by
  treating "has a decision record" as "is decided", and it has its own test.
- The sweep is idempotent, so a daemon polling every few seconds writes one
  audit entry per lapse rather than one per poll.

The Herdr plugin landed just before this and already renders an expired row
correctly: `Verification::Expired` and an empty decision list.

### The remaining work, in the order that unblocks the most

1. **`HRC-TR-001`, `HRC-TR-003`, `HRC-TR-006`** — the out-of-process JSON-RPC
   adapter binding, push-capable adapters, and the GitHub optimization. The
   conformance suite exists and a deliberately broken adapter is already
   proven to fail it, so a new adapter has a target to hit.
2. **`HRC-TECH-011`, `HRC-TECH-012`** — `cargo-dist` release artifacts and a
   clean-host install fixture. `OQ-012` asks whether to pin an exact toolchain
   first; answer it in that pull request.
3. **`HRC-CH-004`** — public repositories after typed confirmation. `OQ-008`
   asks for the warning wording and is unanswered. It needs a human: implement
   the mechanism and leave the wording marked, or ask.
4. **`HRC-CTX-004`, `HRC-CTX-007`** — patch and command-output provenance,
   left open by the context PR pending controlled capture.
5. **`HRC-GOV-004`** — the evidence pass. Every row claiming `Verified` must
   cite something stable. Do this last, once the rows have stopped moving.

`HRC-TECH-001`, `HRC-TECH-003`, `HRC-TECH-005`, and `HRC-TECH-010` are
umbrella rows that close when the work beneath them does. Do not mark them
early.

### Two things a human has to decide

- `AC-ADAPTER-SPEC` carries a product-owner approval gate, which is most of
  why M0 sits at 55% despite the code being there.
- The PRD's product and technical owners are `TBD`.

Neither should be marked done by an agent. Raise them rather than routing
around them.

## 5. What this codebase expects of a change

These are not style preferences; each came from something that went wrong.

**Enforce invariants in types, not conventions.** The agent-safe RPC response
enum has no variant that can hold a message body, so a pending body cannot
leak through it even by mistake (DEC-036, DEC-040). Prefer making a mistake
impossible to documenting that it would be bad.

**Tests should encode the attack, not the happy path.** The valuable tests
here are the ones that try the forgery: a receipt from a device that was never
a recipient, a join proof made for a different invite, an approval replayed, a
backdated message trying to reorder a thread. Write those.

**Verify against something independent.** Device IDs were checked against a
Python JCS implementation, Ed25519 signing against OpenSSL, the safety phrase
against an independently written derivation, the ULID encoding against the
specification's own example. Recording the code's own output as a fixture
proves nothing.

**Fail closed and say what happened.** Every error variant names the specific
thing that was wrong, because a caller showing a tamper alert has to say what
was detected.

**Write comments about the reasoning, not the mechanics.** The house style is
to explain why a thing is done in the order it is, or why the obvious
alternative is wrong. Match the surrounding density.

**When a lint or a test pushes back, consider that it is right.** Clippy's
argument-count lint caught a `deliver` signature with two adjacent `&str`
arguments where transposing them would have delivered unedited content while
the audit recorded the edit. `unsafe_code = "forbid"` forced a better CLI
context design. Both were improvements, not obstacles.

---

## 6. Things that have bitten, so you do not repeat them

**Windows is where the defects are.** Two real bugs in this session were found
only by the Windows job: a `#[cfg(unix)]` function called from unconditional
code (compiles everywhere else, fails to compile on Windows), and a
second-precision timestamp that made ULID ordering depend on randomness —
which passed on Linux only because Git operations happened to take over a
second.

`rustup target add x86_64-pc-windows-msvc` is installed, so
`cargo check -p <crate> --all-targets --target x86_64-pc-windows-msvc` catches
the first class locally. It does not work for crates that depend on
`hrc-storage`, because bundled SQLite needs a Windows C compiler.

On the current machine, even the smallest `cargo check -p hrc-cli --tests
--locked` cannot start because MSVC `link.exe` and the Windows SDK are absent.
Do not install build tools as part of this work; report the blocker and rely
on CI or a provisioned Windows host for Rust test execution.

**CI takes 5 to 16 minutes.** Wait for all five checks — `PRD traceability`,
`Format and lint`, and `Build and test` on ubuntu, macOS, and Windows. The
Windows job is slowest and finishes several minutes after the others; do not
merge on the strength of the first four.

**A job with `runner_id: 0` and no logs never got a runner.** That is
infrastructure, not your code; re-run it. A job that produced logs and failed
is yours.

**Correct a decision when it turns out wrong, in place.** DEC-047 originally
said invite secrets were never stored. Writing `join pending` showed that
verifying a join proof needs exactly that value, so an administrator who
discarded it could never check any request. The decision was amended with the
reason the earlier form was wrong, rather than quietly rewritten. Do that.

**Do not let a stub look finished.** The first draft of that same function
returned `None` unconditionally — it compiled, read plausibly, and would have
made the command silently always empty. If something cannot be implemented
yet, make it report that rather than return a plausible nothing.

---

## 7. Repository map

```
crates/hrc-protocol/     Wire types, canonical JSON, signatures, ULIDs
crates/hrc-crypto/       Ed25519, age, key store, invites, safety phrases
crates/hrc-core/         Roster, messages, enrollment, prompt gate, sync, RPC
crates/hrc-storage/      SQLite state, outbox, inbox ordering, audit
crates/hrc-ipc/          Framed local IPC over sockets and named pipes
crates/hrc-transport/    Adapter trait and conformance suite
crates/hrc-transport-git/Git adapter
crates/hrc-tui/          Trusted approval screen
crates/hrc-cli/          The single `hrc` binary
crates/hrc-herdr/        Herdr plugin entry points (stub)
.agents/skills/          The installable agent skill, held to the CLI by tests
```

Storage migrations are append-only and numbered; add a new file rather than
editing one that has shipped.
