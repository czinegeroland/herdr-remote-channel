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

Milestones M1 to M4 are delivered: a secure channel over a Git repository,
encrypted messaging with receipts and threads, the Herdr plugin, and context
packages with delegation. M0's specification review is still open and M5, other
transports, has not started. Section 30 of the PRD tracks the roadmap and
section 31 the delivery ledger, which every change updates.

## Installing

The Herdr plugin and the `hrc` CLI are the same executable, so one install
gets you both.

### As a Herdr plugin

```bash
herdr plugin install czinegeroland/herdr-remote-channel
```

Herdr clones the repository, shows you the command it is about to run, and on
your confirmation installs the published executable from npm into the plugin
directory. It takes about a second and needs **no Rust toolchain and no
compiler** — the binary is downloaded, not built. npm serves it under its own
integrity hash, and every archive was checked against its published SHA-256
before it was packed.

The version is pinned to the one the manifest declares, so the entry points
Herdr registered and the binary answering them are always the same release.

Node.js is the only prerequisite. This used to be three `cargo` commands, and
that was a mistake: this project had already published to npm precisely so
that nobody would need a toolchain, then left the install path most people
take building from source anyway. On Windows it failed outright with
``linker `link.exe` not found`` — Visual Studio Build Tools, several
gigabytes, to read an inbox pane.

Confirm the install:

```bash
herdr plugin list
```

The plugin registers a startup hook, a `workspace.focused` event hook, an
and seven panes, each with an action of the same name:

- **Remote channel inbox** — a split pane showing what has arrived, who sent
  it, and what is waiting on you. It never shows an unapproved body. It is
  interactive and refreshes on its own, so a message that arrives while you
  are working appears without you touching it: move with the arrows or
  `j`/`k`, filter with `p`/`u`/`a`, and press Enter to open the trusted
  review screen on the message you selected. Nothing in this pane can
  approve, reveal, decline or deliver anything — that is a property of its
  type, not a rule it follows.
- **Remote channel review** — the trusted approval screen, opened as a modal
  popup. This is the one surface where a quarantined body is displayed, and
  the only place a decision about one is made. Once the body is on screen,
  `Tab` chooses which of your local Herdr sessions a delivery would go to;
  the list is what Herdr actually has, the session you are working in comes
  first, and the confirmation names the exact one. Approved text reaches that
  session over Herdr's socket, never as a command-line argument.
- **Remote channel join requests** — admit or refuse someone joining the
  channel, after comparing the safety phrase with them out of band.
- **Remote channel compose** — write a note or a question and send it, or
  answer a message you have received.
- **Remote channel setup** — create a channel, invite someone, or redeem an
  invite code. The code is masked as you type it and never echoed back.
- **Remote channel context** — preview exactly what a context package would
  disclose, then send it. A package with secret-scan findings or excluded
  paths cannot be confirmed.
- **Remote channel members** — remove a member or revoke a device. Removing
  yourself is refused, and revoking someone's last device says what that
  leaves behind.

The review pane needs the daemon running (`hrc daemon`). Revealing a body
takes a deliberate key, every decision takes a second confirming key, and the
confirmation names what is about to happen — so no key press can approve
anything by accident.

`hrc review` opens the same screen in a terminal. Both refuse with
`authorization_required` unless a human is actually there: standard input and
standard output must both be a terminal, which a pipe, a captured subprocess,
and an agent's tool call are not.

The inbox pane raises a Herdr notification when something new needs you, and
raises it **once**: the record of having notified lives in the local database,
so refreshing the pane does not repeat it and neither does restarting. A burst
of ordinary arrivals becomes one line with a count; a tamper halt or a prompt
request is never folded into one. A notification carries locally resolved
names, counts and fixed wording — never a subject, a body, an excerpt or an
attachment name — and cannot approve, reveal, decline or deliver anything.

Reading the pane non-interactively — `hrc herdr pane inbox --json`, which is
what an agent or a script gets — returns the same rows as JSON. It is the same
closed metadata set either way.

### For local development

```bash
git clone https://github.com/czinegeroland/herdr-remote-channel
cd herdr-remote-channel
cargo install --path crates/hrc-cli --root . --locked --force
mkdir -p node_modules && ln -sfn ../npm/hrc node_modules/herdr-remote-channel
herdr plugin link "$PWD"
```

On Windows, make the link with
`mklink /J node_modules\herdr-remote-channel npm\hrc` instead of `ln`.

This is where a source build belongs, and it is the one flow that needs a
Rust toolchain. `herdr plugin link` does not run build commands, which is why
the install is invoked by hand first; re-run it after any change to the Rust
sources. Every action in the manifest starts
`node node_modules/herdr-remote-channel/bin.js`, which in an installed plugin
is the published npm package. The link points that path at the shim's source
instead, and the shim, seeing that it is running from `npm/hrc` in a
checkout, runs the `bin/hrc` that `--root .` wrote rather than looking for a
published binary.

### The CLI on its own, with no toolchain

The CLI is on npm and needs no Rust:

```bash
npx herdr-remote-channel@latest --version
```

**Use the full package name.** `npx hrc` installs an unrelated package that
already owns that name on npm and runs someone else's code. The executable is
called `hrc` once installed — it is only the `npx` shorthand that is unsafe:

```bash
npm install -g herdr-remote-channel
hrc --version
```

The binary is not downloaded when you install. Each platform's executable is
published inside its own npm package and selected by npm's `os` and `cpu`
constraints, so npm serves the bytes under its own integrity hash and nothing
is fetched from GitHub at install time. Every archive is checked against its
published SHA-256 before it is packed, and a mismatch stops the publish.

There is also a shell installer, which does the same verification itself:

```bash
curl -fsSL https://raw.githubusercontent.com/czinegeroland/herdr-remote-channel/main/scripts/install.sh | sh -s -- --version v0.2.3
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
| Startup | `hrc herdr startup` — places the inbox split, unless configured not to |
| Event `workspace.focused` | `hrc herdr event` — refreshes the sidebar |

Every pane also has an action of the same name, so each screen can be opened
from Herdr's action list in workspace and pane contexts.

| Pane | Placement | What it is for |
|---|---|---|
| `inbox` — Remote channel inbox | split | Watching what arrives; `Enter` reviews a message, `t` opens its thread |
| `review` — Remote channel review | popup | Reading a quarantined body and deciding: deliver, edit then deliver, keep, or decline with a reason |
| `thread` — Remote channel thread | zoomed | Reading one conversation, showing only content a human already released |
| `compose` — Remote channel compose | popup | Writing a note, question or reply |
| `context` — Remote channel context | popup | Previewing and disclosing a context package |
| `setup` — Remote channel setup | popup | Creating a channel, inviting, joining |
| `joins` — Remote channel join requests | popup | Admitting a member after the safety phrase matches |
| `members` — Remote channel members | popup | Naming, removing, and revoking members and devices |

One link handler is registered: a Ctrl-click on the join link that `hrc
invite create` prints (`<locator>#hrc-join`) opens the setup screen on the
join step. It matches only that fragment, so ordinary repository links keep
opening in the browser, and it never carries the invite code.

The event subscription is deliberately one entry long. PRD section 13.2 puts
frequently occurring event processing in the daemon rather than in repeatedly
spawned hook commands, and every name added there is a process spawn at the
host's rate rather than ours. Herdr names the event in `HERDR_PLUGIN_EVENT`;
an event this plugin did not subscribe to is ignored rather than failed, so a
host that delivers one does not show you a broken plugin.

Nothing the plugin can be asked to do approves, delivers, or reveals remote
content. Approval is a human act on the trusted screen (PRD section 19.2),
and the reaction type has no variant that could perform one.

## Configuring it

Herdr gives every plugin a configuration directory and names it in
`HERDR_PLUGIN_CONFIG_DIR`. Write `config.json` there:

```json
{
  "inbox": {
    "open_at_startup": true,
    "share": 0.25
  },
  "notifications": {
    "enabled": true
  },
  "indicator": {
    "pane_token": true,
    "window_title": false
  },
  "delivery": {
    "wait_for_idle": true
  }
}
```

| Setting | Default | What it does |
|---|---|---|
| `inbox.open_at_startup` | `true` | Whether the startup hook places the inbox split. Turn it off to open the inbox yourself from the `Remote channel inbox` action |
| `inbox.share` | `0.25` | The share of its split the inbox takes, between `0.1` and `0.9` |
| `notifications.enabled` | `true` | Whether notifications are raised. The ledger is written either way, so turning them back on does not replay what arrived while they were off |
| `indicator.pane_token` | `true` | Whether a count is reported beside the inbox pane in Herdr's own sidebar |
| `indicator.window_title` | `false` | Whether a count is written to the terminal window title. Off by default: the title belongs to the client, and anything else that sets it will be overwritten |
| `delivery.wait_for_idle` | `true` | Whether an approved message to an agent in the middle of a turn waits until it is idle, for at most ten minutes, rather than landing mid-turn |

Every setting is optional and a missing file is the ordinary case. A file
that cannot be parsed, a value outside its range, and a key this build does
not recognize all leave the defaults in place and are **reported** in the
startup hook's answer, which is what `herdr plugin log list` shows — a
setting that silently does nothing is the thing that wastes an afternoon.

Nothing here is security-relevant and nothing here may become so. The file is
ordinary user-editable text with no signature: it configures placement and
volume, never whether a gate applies, who may decide, or what a surface may
show.

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

