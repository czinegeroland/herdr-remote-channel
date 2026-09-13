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

### State

74 of 122 requirement rows Implemented. Milestones: **M0 55%, M1 100%,
M2 99%, M3 80%, M4 65%, M5 0%.**

Thirty pull requests merged. The working system today: two installations can
create a channel, invite, join, compare safety phrases, admit a member through
the trusted interface, exchange encrypted notes and threaded questions and
answers over a Git remote, and have everything arrive quarantined behind the
prompt gate.

### In flight

**PR #30 is open and not merged.** It carries messaging wired end to end plus
membership operations through the trusted interface. CI was re-run after the
Actions quota was topped up; check it, and if green, squash merge it. Its
content is verified locally — full suite, clippy, and fmt clean.

If it is red for a reason in the diff, fix it. Do not close it and start over;
it is a large and coherent change.

### The immediate next pieces

In roughly the order that unblocks the most:

1. **`HRC-CH-010`, `HRC-SYNC-008`** — tamper halt proven over a real
   repository, and re-encryption of queued messages after a roster epoch
   change. The sync core already halts; what is missing is the end-to-end
   proof and the re-encrypt path.
2. **`HRC-CTX-005`, `HRC-CTX-007`, `HRC-SKILL-006`, `HRC-SKILL-008`** —
   context packages reachable from the CLI, received context quarantined like
   a message body, and git-ignored paths excluded. `excluded_path_reason`
   handles the path-decidable rules; the git-ignored check needs the
   repository.
3. **`HRC-MSG-005`** — expiry display and sweeping. Opening already refuses an
   expired message.
4. **`HRC-TR-001`, `HRC-TR-003`, `HRC-TR-006`** — the out-of-process JSON-RPC
   adapter binding, push-capable adapters, and the GitHub optimization. The
   conformance suite exists and a deliberately broken adapter is already
   proven to fail it, so new adapters have a target to hit.
5. **`HRC-TECH-002`** — the Herdr plugin entry points, still the documented
   not-implemented code.
6. **`HRC-TECH-011`, `HRC-TECH-012`** — `cargo-dist` release artifacts and a
   clean-host install fixture. `OQ-012` asks whether to pin an exact toolchain
   first; answer it in that PR.
7. **`HRC-CH-004`** — public repositories after typed confirmation. `OQ-008`
   asks for the warning text and is unanswered; it needs a human, so either
   ask or implement the mechanism and leave the wording marked.
8. **`HRC-GOV-004`** — the evidence pass: every row claiming `Verified` must
   cite something stable. Do this last, when the rows have stopped moving.

`HRC-TECH-001/003/005/010` are umbrella rows that close when the work under
them does. Do not mark them early.

---

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

**CI takes 4 to 8 minutes.** Wait for all five checks — `PRD traceability`,
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
