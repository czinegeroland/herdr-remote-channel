# Refactor PRD

**Status:** Accepted for execution · **Date:** 2026-09-23 · **Baseline:** `main` at `ac4144a`

This is the plan for a whole-repository review and the refactor that follows
from it. It sits beside `docs/PRD.md` rather than inside it: the product
requirements do not change, and nothing here adds a feature. Every change it
calls for either fixes a defect the review found or makes the code easier to
change without changing what it does.

The living-PRD policy still applies. Each pull request below updates
`docs/PRD.md`: its delivery ledger and timestamp always, and a requirement's
evidence row when a defect in that requirement is fixed. A pure move names no
requirement and says so with a `No requirement progress:` line, which the
traceability check accepts.

---

## 1. Principles

1. **Behaviour is preserved unless a defect is being fixed, and a fix says
   so.** A refactor PR changes no command's output, exit code, JSON shape,
   storage schema, or wire format. When one has to, it is a defect fix,
   named as one, with its own PR.
2. **Defects first.** A structural cleanup that moves broken code moves it
   broken. The two defect PRs land before any file is split.
3. **One concern per PR.** A reviewer should be able to read a move as a
   move. Mixing a move with a change hides the change.
4. **CI is the proof.** Every PR goes green on the three-platform matrix,
   the living-PRD check and the end-to-end suite before it merges. Nothing
   here is verified by running suites locally.
5. **No public API churn for its own sake.** Crate boundaries stay. What
   moves is inside `hrc-cli`, `hrc-storage` and the time helpers.

---

## 2. What the review found

Measured on 34,106 non-test lines across ten crates.

### 2.1 Defects

**D1 — Every approved prompt names the wrong channel.**
`DaemonBroker::channel_local_name` (`crates/hrc-cli/src/commands.rs:3722`)
ignores its argument and returns the literal `"local channel"`. Its only
caller is `dispatch_trusted`'s approval path, which passes it to
`gate::deliver`, which writes it into the provenance banner of every body
handed to an agent (`crates/hrc-core/src/gate.rs:402`). The banner is
requirement HRC-GATE-005 — the frame telling an agent that what follows
came from another machine. It has said `Channel: local channel` on every
delivery since it was written. This is the seventh instance of the pattern
this project keeps finding: code that exists, is tested in isolation, and
is wired to a placeholder at the one seam no test crosses.

**D2 — `hrc doctor` exits 0 when a check fails.**
`doctor` returns `Ok` with `"healthy": false`, so a script or an agent
branching on the exit code is told everything passed. Known since the first
round of manual testing, never fixed.

**D3 — The local principal was read from the device key.** Found while
executing R4, and the reason R4 is a defect fix rather than a pure refactor.
`hrc init` generates two keys, a principal and a device, and the roster lists
principals. The membership screen, the compose screen and the reply targets
all asked `whoami` for "the local principal" and read back `signingKey`,
which is the device's key; the daemon built its agent-safe `whoami` answer
the same way. No roster entry ever matched. The membership screen never
recognized its own row, so its refusal to remove yourself never applied (the
core rule that the sole administrator cannot be removed still did); compose
offered this installation as its own recipient; and an agent asking the
daemon who it was got the device key under the principal's name. The
end-to-end log had printed both values side by side, unequal, on every run.
This is S2 in practice: the value was not missing, it was the wrong field,
and a string key cannot say which.

### 2.2 Structural

**S1 — `commands.rs` is 4,846 lines holding about a dozen concerns.**
Identity, channel creation and publication, enrolment, membership,
receiving and synchronization, composing and sending, context capture
(about 530 lines of its own), reading and waiting, civil-date arithmetic,
diagnostics, the daemon with its 460-line broker, and the Herdr entry
points. Nothing about them is shared except that they are commands.

**S2 — Internal callers parse the CLI's own JSON back by string key.**
Commands return `serde_json::Value`, which is right at the edge where it is
printed. But the TUI drivers in `commands/review.rs` call those same
commands and read fields back out:

```rust
let local = crate::commands::whoami(context)?;
let local_principal = local["signingKey"].as_str().unwrap_or_default();
```

This appears three times, plus `members(context)?["members"]` and
per-member `["principalId"]`. A renamed key does not fail to compile; it
yields an empty string. On the members screen, an empty local principal
means no row is recognized as this installation, so the screen's refusal to
remove yourself stops applying. The daemon refuses that operation too, so
the defence holds in depth — but one layer of it currently rests on string
keys matching.

**S3 — The same date arithmetic exists twice, and the same formatter
twice.** `days_from_civil` is in `crates/hrc-cli/src/commands.rs:2783` and
`crates/hrc-tui/src/inbox.rs:166`; RFC 3339 parsing is written separately in
each (`time_from_rfc3339`, `epoch_seconds`). The "12s ago" formatter
`elapsed` is in both `crates/hrc-tui/src/inbox.rs:133` and
`crates/hrc-herdr/src/sidebar.rs:100`, with the same thresholds and a
different suffix.

**S4 — Error-conversion boilerplate in the daemon broker.**
`.map_err(|error| hrc_core::CoreError::Transport(error.to_string()))`
appears 19 times in `commands.rs`, nearly all in `impl Broker for
DaemonBroker`.

**S5 — `hrc-storage/src/lib.rs` is 3,396 lines: 59 public methods and 18
public types in one file.** Channels, the outbox and its reseal machinery,
the inbox and ordering, receipts, decisions and audit, invites, context
packages, notifications and aliases are all methods on one `impl Database`.

**S6 — `commands/review.rs` is 1,811 lines driving seven unrelated
screens.** Review, join requests, compose, setup, context, members and the
inbox side view each have their own driver and helpers and share only the
runtime plumbing at the top of the file.

**S7 — Dead code on the daemon's agent-safe surface.**
`DaemonBroker::record_draft` returns `"unavailable"` and `observe` returns
`None`. Neither is reachable: `handle_agent_request` answers `draft` and
`wait` with `unimplemented` before the broker exists. The CLI implements
both directly against storage. So the protocol advertises two methods that
one of its two implementations cannot serve.

### 2.3 Noted, and out of scope

**O1 — Half the code assumes one channel.** `only_channel` returns
`AmbiguousChannel` when an installation holds more than one, and is called
from 26 places; storage, the sidebar, notifications and the inbox iterate
`channels()`. Resolving this is a product decision — whether an
installation may belong to several channels — not a refactor. Recorded as
open question OQ-016.

**O2 — Test files are large** (`command_contract.rs` 3,604 lines,
`storage/src/tests.rs` 2,923). They follow the code they test; splitting
the code will make splitting them natural, and doing it separately first
would be churn.

---

## 3. Plan

Ordered so that each PR is small, independently mergeable, and leaves
`main` green. Defects first, then the pieces the moves depend on, then the
moves.

| PR | Scope | Kind | Behaviour change | Status |
|---|---|---|---|---|
| R1 | Fix D1: the broker resolves the message's real channel name | Defect | Provenance banner names the channel | Merged in #87 |
| R2 | Fix D2: `hrc doctor` exits non-zero when a check fails | Defect | Exit code only | Merged in #88 |
| R3 | S3: one `hrc_core::time` module; the CLI, the TUI and the sidebar use it | Structural | None | Merged in #89 |
| R4 | S2: typed `whoami` and `members` results; the CLI serializes them at the edge and the TUI drivers stop parsing JSON | Defect (D3) + structural | Screens and daemon read the principal, not the device key | Merged in #90 |
| R5 | S4: move the daemon and its broker to `commands/serve.rs` and add one conversion helper. S7 deferred, see section 3.6 | Structural | None | Merged in #91 |
| R6 | S1: split the rest of `commands.rs` into `commands/{identity,channel,enrol,membership,sync,send,context,read,diagnostics,herdr}.rs`, re-exported so no caller changes | Structural | None | Merged in #92 |
| R7 | S6: split `commands/review.rs` into one module per screen under `commands/review/` | Structural | None | Merged in #93 |
| R8 | S5: split `hrc-storage/src/lib.rs` into modules by table area, keeping one `impl Database` spread across them | Structural | None | In review |

### 3.1 R1 — the channel on the provenance banner

`DaemonBroker` already holds the context and opens the database; the
message identifier it is given is enough to find the inbox row and so the
channel. The fix resolves the name through `channel_display_name`, which
the pane and notifications already use, so the banner, the inbox and the
toast agree on what the channel is called.

Evidence it closes: a test in `crates/hrc-cli/src/commands/tests.rs` that
approves a real message through `dispatch_trusted` with a `DaemonBroker`
over a real channel and asserts the framed content names that channel —
the seam no existing test crosses, which is why the stub survived.

### 3.2 R2 — doctor's exit code

A failed check exits 1 — section 22.8's "the command failed at runtime",
an existing code rather than a new one — while printing exactly the same
list of checks it prints today. A person still reads which check failed; a
script or an agent branching on the exit code is no longer told that
everything passed.

### 3.3 R3 — one time module

`hrc_core::time` holds `days_from_civil`, `civil_from_days`, RFC 3339
parsing in the fixed shape `Database::utc_now` writes, and `elapsed`. The
two `elapsed` copies differ only in suffix, so the shared one returns the
duration and each caller adds its own word. `hrc-core` because it is the
lowest crate all three callers already depend on.

### 3.4 R4 — typed results inside, JSON at the edge

`whoami` and `members` gain typed inner functions returning structs; the
existing `Result<Value>` commands become thin serializers over them, so the
printed JSON is byte-identical. The TUI drivers call the typed functions.
Only these two, because they are the ones read back — converting every
command would be a much larger change for no one's benefit.

### 3.5 R5–R8 — the moves

Each is `git mv`-shaped: items relocate, `pub use` keeps every existing
path valid, and no function body changes except for imports. The target is
that no non-test source file exceeds 1,500 lines. Where a function is
private and now needed across modules it becomes `pub(super)`, never
`pub`.

---

### 3.6 What R5 did not do, and why

R5 was planned to delete the two unreachable broker stubs of finding S7. It
does not. `record_draft` and `observe` are required methods of
`hrc_core::rpc::Broker`, because the agent-safe protocol advertises `draft`
and `wait`; the daemon answers both with `unimplemented` before its broker
exists, and the CLI implements both directly against storage. Deleting the
stubs therefore means one of two protocol changes: implement `draft` and
`wait` in the daemon over the functions the CLI already uses, or remove
them from the agent-safe protocol. Either is a change to what the daemon
promises, not a move, so it is recorded here rather than done inside a
refactor that promises to change nothing.

The module is `serve.rs` rather than the planned `daemon.rs`, because
`commands::daemon` is already the name of the function that runs it.

## 4. Acceptance

The refactor is done when:

1. R1 through R8 are merged, each green on the full CI.
2. The provenance banner names the real channel, proven by a test that
   crosses the broker seam.
3. `hrc doctor` exits non-zero on a failed check.
4. No function in `hrc-cli` reads a field of a `Value` returned by another
   `hrc-cli` command.
5. `days_from_civil`, RFC 3339 parsing and `elapsed` each exist once.
6. No non-test source file is longer than 1,500 lines.
7. No command's printed output, exit code on success, storage schema or
   wire format changed except as stated for R1 and R2.

## 5. Risks

| Risk | Mitigation |
|---|---|
| A move silently changes behaviour | Moves carry no body changes; the full contract and end-to-end suites run on every PR |
| A long-lived refactor branch conflicts with feature work | One PR at a time, each merged before the next starts |
| Visibility widens as items move | `pub(super)` by default; any new `pub` item is called out in its PR |
| The storage split reorders SQL or transactions | R8 moves whole methods only; no statement is edited |
