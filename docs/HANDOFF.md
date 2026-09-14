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

**94 of 94 requirement rows Implemented.** Milestones: **M0 90%, M1 100%,
M2 100%, M3 100%, M4 95%, M5 0%.**

`main` carries the whole PRD. Nothing is in flight.

The plugin is installable as of decision DEC-053. `herdr-plugin.toml` at the
repository root is Herdr's own schema, generated by `hrc herdr manifest` and
held to the binary by `crates/hrc-cli/tests/plugin_manifest.rs`. Before this,
section 13.2's "generated manifest" was generated into a shape of our own
invention that no host reads, and the event hook read stdin while Herdr names
the event in `HERDR_PLUGIN_EVENT`. Neither would have fired.

The install-time build steps are three `cargo` invocations and no shell
script at all (DEC-057). They began as one script per platform, and the
PowerShell half failed on the first real Windows install while the POSIX half
worked — two implementations of one idea, only one of which anyone had run.

The section 28.5 scenario now runs as one test,
`the_end_to_end_scenario_from_section_28_5` in
`crates/hrc-cli/tests/command_contract.rs`, over two independent
installations and one bare Git remote. Writing it was worth doing for what
it found rather than for what it confirmed: the other messaging tests use a
single-installation fixture where sender and recipient are the same device,
so nothing had ever encrypted to a key the process did not already hold.
Driving a real two-party exchange turned up two agent-safe daemon methods
still answering `unimplemented` — `inbox` and `show_approved`, which are
exactly what the Herdr plugin talks to a running daemon for. Both are now
served. `draft` and `wait` are still unimplemented on that surface, and so
are the `hrc show`, `hrc wait`, and `hrc delegate` commands.

What works today, end to end and proven by tests against a real Git
repository: two installations create a channel, invite, join, compare
matching safety phrases, admit a member through the trusted interface,
exchange encrypted notes and threaded questions and answers, draft and send
source-derived context packages behind a one-use trusted authorization, and
have everything arrive quarantined behind the prompt gate. The Herdr plugin
renders its manifest, sidebar, inbox and notifications. Messages carry an
optional expiry and lapsed ones are swept. Observed history rewrites halt
synchronization stickily. Making a repository public needs the full section
16.4 disclosure and a typed phrase naming the channel. The daemon stops on a
signal and releases its endpoints. `cargo dist` plans the four required
targets and builds a checksum-verified artifact that installs and runs on a
host with no toolchain.

### What is left, and none of it is a requirement row

- **Cut a first tag.** The Windows and macOS artifacts exist only once a
  tagged release runs. The pipeline is `cargo dist` and has been exercised
  locally for Linux, but no published artifact has been verified.
- **`AC-ADAPTER-SPEC`** carries a product-owner approval gate. The code and
  its conformance suite are there; the specification review is not an
  agent's to record.
- **`OQ-008`'s wording.** The public-repository mechanism is done and the
  seven disclosure strings are marked provisional in
  `crates/hrc-core/src/visibility.rs`. The tests assert structure rather
  than prose, so every string can be replaced without breaking anything.
- **`DEC-051`** is Proposed: whether to add keychain storage on Windows and
  macOS. `DEC-052` explains why the requirement does not depend on it.
- **The product and technical owners are still `TBD`.**
- **M5** is deliberately 0%: deferred until the core protocol stabilizes.
- **Four surfaces answer `unimplemented`.** The agent-safe daemon's `draft`
  and `wait`, and the `hrc show`, `hrc wait` and `hrc delegate` commands. No
  requirement row claims them, which is why the count still reads 94 of 94 —
  but a plugin or an agent that calls them gets an error, so they are worth
  closing before a first tag.
- **`OQ-014`** is new and open: whether a sender should see its own messages
  in `hrc thread`. Today it sees only what arrived, because an installation
  keeps ciphertext and a payload hash for what it sent, not plaintext.
- **Nothing has been run against a real Herdr.** The manifest matches the
  0.8.0 plugin reference and every command in it is exercised by tests, but
  no `herdr plugin install` has been performed. The most likely thing to be
  wrong is a detail the reference does not state: whether `bin/hrc` resolves
  without an `.exe` suffix on Windows, and whether `contexts = ["pane"]` is
  accepted.

### Two operational things the next session must know

**Actions is free now: the repository is public**, and standard
GitHub-hosted runners carry no per-minute charge on public repositories.
Prefer CI over a local run wherever both are available — it covers three
platforms and a local run covers one. What follows is the local equivalent,
still correct and still what to use when CI is unavailable.

Two CI outages happened during this work and both were misread at first, so:
a run that fails in seconds with `startup_failure` on *every* workflow is an
Actions **permissions** problem (Settings, Actions, General), not billing.
The way to tell them apart is a workflow with no `uses:` at all — if that one
runs, the runners and the billing are fine and an action is being refused.

**Actions spending was exhausted from #42 to #50,** while the repository was
still private. From pull request #42 onward every
job failed in seconds with no runner assigned — that signature is billing,
not code. The user's instruction was to run the checks locally and merge on
a clean pass, which is what the last several pull requests did. Since #38
reduced pull requests to Linux only, a local run covers exactly what pull
request CI would have, not a subset.

The local equivalent, in order:

```bash
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTFLAGS="-D warnings" cargo test --workspace --all-targets --locked
RUSTFLAGS="-D warnings" cargo test --workspace --doc --locked
```

The PRD validator is PowerShell. Install pwsh and run the real script rather
than reimplementing it; synthesize the event payload it reads the pull
request body from. Doing this before opening a pull request has caught a
missing ledger update, a requirement ID claimed without a row change, and
example IDs mentioned in prose — each of which would otherwise have cost a
CI cycle.

**Windows and macOS run on every pull request again** (decision DEC-056).
They were Linux-only from #38 to #56 because the repository was private and
billed; going public made standard runners free and the matrix came back,
and `cross-platform.yml` is deleted rather than left to drift beside it.

That gap cost something concrete, which is worth remembering rather than
filing away: `herdr/build.ps1` shipped broken because Windows PowerShell
turns a redirected native command's stderr into a terminating error. No pull
request ran Windows, and PowerShell 7 relaxed that behaviour so it could not
be reproduced locally either. Windows has now caught three real defects
here.

### Four things a human has to decide

- `AC-ADAPTER-SPEC` carries a product-owner approval gate. The code and its
  conformance suite are there; the specification review is not an agent's to
  record.
- Whether the passphrase store alone satisfies `HRC-SEC-001`, and whether to
  accept decision DEC-051 on keychain backends.
- Whether the hand-written release workflow is acceptable in place of a
  `cargo dist`-generated one, which decision DEC-015 names.
- `OQ-008`'s public-repository warning wording. The mechanism is done and the
  seven strings are marked provisional in `crates/hrc-core/src/visibility.rs`;
  the tests assert structure rather than prose, so every string can be
  replaced without breaking anything.
- The PRD's product and technical owners are still `TBD`.

None of these should be marked done by an agent. Raise them rather than
routing around them.

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
