# Landscape research: where Herdr Remote Channel actually sits

**Date:** 2026-09-20 · **Against:** `docs/PRD.md` as of `de027ec` (v0.2.5)

This is research, not a requirement change. Nothing here edits the PRD. It
exists to answer four questions asked of the product: what is missing, what
people actually want from Herdr plugins, what the state of remote messaging
between agentic sessions is, and what to build next.

Every claim that came from outside this repository carries its source.

---

## 1. The one-paragraph finding

The product's thesis is correct and now has external evidence behind it, but
the product is **invisible**, it is **under-using the host it was built for**,
and it is **positioned inside a crowd it does not belong to**. The protocol
work is far ahead of the surface area. The cheapest wins are not features —
they are a GitHub topic, four unused Herdr API calls, and a name that stops
colliding with five other plugins that do something else.

---

## 2. Evidence that the problem is real

The PRD's problem statement (§2) was written from first principles. It now has
measurements behind it.

**CooperBench** (600 collaborative coding tasks, 12 libraries, 4 languages)
found a ~30% average drop in success when two agents cooperate versus working
alone; GPT-5- and Claude-Sonnet-4.5-based agents land around 25% on two-agent
cooperation, roughly half the solo baseline. The paper's subtitle is "Why
Coding Agents Cannot be Your Teammates Yet." Its three failure modes are worth
quoting because each one maps onto a concrete gap in our protocol:

| CooperBench failure mode | Share | What HRC has | What HRC is missing |
|---|---|---|---|
| **Communication failures** — questions go unanswered, messages arrive too late to inform a decision, channels flood with repetitive status updates | 26% | threads, expiry, receipts, notification coalescing | nothing tracks an *unanswered* question; nothing times delivery to the recipient's state |
| **Commitment failures** — agents break promises or make unverifiable claims | 32% | `task` / `accept` / `progress` / `result` states (§18.4) | a `result` carries prose. Nothing makes a claim *checkable* |
| **Incorrect expectations** — agents hold wrong beliefs about others' plans and observations | — | context packages (§20) | no shared plan object; context is a one-shot push, not a shared surface |

Agents were measured spending **up to 20% of their budget on communication**,
which reduced merge conflicts but did not improve success. That is the single
most important number in this document: *more messaging is not the product*.
Better-shaped, verifiable, well-timed messaging is. The PRD's principle 1
("inbound messages are data, not instructions") is already the right instinct;
the gap is that we have no way to say "this claim is true" or "you never
answered me."

Maggie Appleton's *One Developer, Two Dozen Agents, Zero Alignment* names the
same thing from the human side: coordination debt, uncontextual PR review
queues, and a planning phase that collapsed because implementation got cheap —
pushing all coordination to the end, via pull requests, when it is too late.
Her diagnosis is that GitHub, Slack and Jira are "not designed for the agentic
development world."

The Herdr Hacker News thread confirms the demand from the other direction.
Commenters ask directly whether "each agent can be in a different SSH session,"
praise the notifications when agents await input, and complain that they cannot
see *what* each agent is executing or where it is stuck. One describes a dozen
terminal windows becoming unmanageable.

**Read together:** people are already running agents on several machines, they
already want to know who is blocked, and the measured bottleneck is
communication quality rather than coding ability. That is precisely HRC's
territory — and nobody is standing in it.

---

## 3. Competitive position: we are in the wrong crowd

The Herdr marketplace lists **1,218 plugins across 1,196 repositories**. A
search for the obvious neighbours returns a dense cluster:

| Plugin | What it actually does |
|---|---|
| `dcolinmorgan/herdr-remote` | menu bar / phone / Telegram dashboard, Cloudflare tunnel, one-tap approvals, remote terminal interaction |
| `dcolinmorgan/herdr-push` | zero-dep event push to herdr-remote for mobile monitoring |
| `0cv/herdr-mobile-relay` | phone approval + monitoring, push notifications, QR setup, multi-computer relays |
| `barnuri/herdr-web`, `yuloop/herdr-web` | mobile-first web UI, drive agents from a phone, Web Push |
| `cobanov/herdr-ntfysh` | ntfy push when an agent finishes or needs input |
| `tomoya55/cmux-herdr` | propagates agent status to the cmux sidebar |

**Every one of these is one person reaching their own machines.** Not one of
them is two *different people*, with *separate keys*, exchanging *consented*
messages. They are remote control; the PRD's §1 says in its second paragraph
that HRC is explicitly not that.

Three consequences:

1. **The name is working against us.** `herdr-remote-channel` sits
   alphabetically and semantically next to `herdr-remote` and
   `herdr-mobile-relay`, which are a different product. A reader scanning the
   marketplace will file us as "another phone dashboard." The one-line
   description — "Secure asynchronous communication between independent Herdr
   installations" — does not contain the words that distinguish us: *two
   people*, *end-to-end encrypted*, *no remote control*.
2. **The differentiator is real and defensible.** Those plugins ship a relay
   service, a tunnel, and a token. We ship per-device keys, a signed roster,
   an append-only Git history, and a human gate. That is a *security* story
   nobody else in this marketplace is telling.
3. **There is an adjacent, unserved need** the cluster reveals: all of them
   exist because people want to know when an agent is blocked. HRC already
   models "blocked" — as a prompt request needing a human. We are one step from
   "tell my teammate my agent is stuck," which none of them do.

### 3.1 We are not in the marketplace at all

`GET /repos/czinegeroland/herdr-remote-channel/topics` returns `{"names": []}`.

The marketplace index picks up public repositories carrying the GitHub topic
**`herdr-plugin`** whose default branch contains a parseable
`herdr-plugin.toml`. We have the manifest. We do not have the topic. **We are
invisible in a 1,218-plugin marketplace**, and the fix is one API call.

This is the highest value-to-effort item in this entire document.

---

## 4. Herdr surfaces we are not using

The manifest supports `build`, `startup`, `actions`, `events`, `panes` and
`link_handlers`. The socket API is much larger than the dozen methods we call.
Below is what exists and what we do with it.

### 4.1 Manifest

| Surface | Status | Opportunity |
|---|---|---|
| `[[build]]` | used | — |
| `[[startup]]` | used (places the inbox) | — |
| `[[actions]]` × 7 | used | `contexts` is only `workspace`/`pane`; worth checking whether an agent context exists |
| `[[events]]` | **one hook, `workspace.focused`** | see 4.2 — we subscribe to the least informative event available |
| `[[panes]]` | used, all five placements available (`overlay`, `popup`, `split`, `tab`, `zoomed`) | `zoomed` is unexplored and is the natural placement for a thread reader |
| `[[link_handlers]]` | **unused** | see 5.2 — this is the missing enrollment UX |

### 4.2 Events we could hook but do not

Herdr emits: `workspace.created/updated/metadata_updated/renamed/moved/reordered/closed/focused`,
`tab.created/closed/focused/renamed/moved`,
`pane.created/updated/closed/focused/moved/exited/agent_detected/output_matched/agent_status_changed/scroll_changed`,
`layout.updated`, and `worktree.created/opened/removed`.

We hook **`workspace.focused`** and poll on a one-second tick.

`pane.agent_status_changed` is the one that matters. It is the event that tells
us a local agent became blocked, or finished. Two features fall straight out of
it (see 5.1 and 5.4), and it also removes the need to poll.

`worktree.created` is the second: it is the moment a delegation becomes real
work.

### 4.3 Socket methods that unblock things the PRD already deferred

| Method | Why it matters here |
|---|---|
| `server.agent_manifests` | **This closes DEC-091.** Creating a new pane as a delivery destination was deferred because it "needs a choice of agent kind." This method *is* the list of agent kinds. The deferral is no longer blocked on a missing capability — only on a product decision about defaults. |
| `agent.start` | the other half of DEC-091: open a fresh agent to receive an approved delegation |
| `agent.wait` | Herdr's own docs describe waiting "until another agent is genuinely blocked." This is the timing primitive for 5.4 |
| `pane.report_metadata` (`herdr pane report-metadata --token name=value`) | OQ-015 noted this and DEC-096 chose the pane border instead. That was right for a *halt reason*, which a token cannot carry. It is **not** right for a count. Both can be true: the border carries the sentence, a token carries `hrc=2!` beside the pane in Herdr's own Agent sidebar. We resolved OQ-015 as either/or when it is both |
| `client.window_title.set` | sets the **terminal window / OS title**. `⚑2 · hrc` in the title bar is visible when Herdr is not the focused window and costs one call. This is the ambient indicator §23.1 wanted and could not find |
| `pane.graphics.set` | images in a pane. A QR code for an invite (see 5.2); a sparkline of channel activity |
| `events.subscribe` / `events.wait` | push instead of the 1s poll |
| `layout.export` / `session.snapshot` | a context package could carry *the shape of the workspace*, not just files |
| `worktree.create` | accept a delegation *into an isolated worktree* — the standard fix for concurrent agents, per the 2026 multi-agent literature |
| `pane.output_matched` / `pane.wait_for_output` | detect an agent asking a question it cannot answer locally, and offer to ask the channel |

---

## 5. Feature ideas, ranked

Ranked by (evidence it is wanted) × (fit with the PRD's non-goals) ÷ effort.
Every one of these respects §5: none of them execute anything remotely, none
bypass the human gate.

### 5.1 Blocked-agent broadcast — *"my agent is stuck, and you can see it"*

**Evidence:** the single most-served need in the whole plugin cluster (§3);
HN commenters want to know "where they're getting stuck"; CooperBench's 26%
communication failures are largely questions that arrive too late.

Hook `pane.agent_status_changed`. When a local agent enters a blocked state
and stays there past a threshold, offer — never automatically — to publish a
`note` to the channel saying *an* agent is blocked. Locally resolved names,
counts, fixed wording; nothing the agent wrote, exactly as §23.3 requires of
notifications. The teammate's inbox shows `alice · blocked 8m`.

This is the first feature that makes a channel useful when nobody has typed
anything. It converts HRC from a messaging app into a **presence layer for
work**, which is what the crowd in §3 is fumbling towards with tunnels.

Risk to manage: this is a presence signal, and §5 lists "real-time presence
guarantees" as a non-goal. Frame it as an *event*, not presence — it is a
message about a state change, published once, through the same gate.

### 5.2 `link_handlers` for invites — the enrollment UX we never built

**Evidence:** §15 enrollment is implemented and, by the user's own account,
the hardest part to walk someone through. `[[link_handlers]]` is completely
unused.

Register a handler for a `herdr-channel:` / `hrc+invite:` pattern. A teammate
pastes an invite link into any pane; a modified click opens the join popup with
the invite pre-filled. The invite *secret* still travels out of band, as §15
requires — the link carries the locator and the invite id, not the secret.

Pair it with `pane.graphics.set` rendering the invite as a QR code, which is
how `herdr-mobile-relay` does its setup and is demonstrably the pattern people
expect in 2026.

This is a small, self-contained, high-visibility feature that closes the
product's worst usability gap.

### 5.3 Verifiable results — the answer to 32% commitment failures

**Evidence:** CooperBench's largest single failure mode is agents breaking
promises and making **unverifiable claims**. §18.4 gives us `result` messages
that carry prose.

Let a `result` (and a `progress`) carry a **checkable reference**: a commit
SHA, a tree hash, a test name, a CI run id. The receiving side does not execute
anything — it *displays whether the reference resolves* in the receiver's own
checkout. `bob · result · 3f2a91c ✓ resolves` versus `✓ claimed, not found`.

This is squarely inside §5's non-goals — no remote execution, no automatic
merging, no automatic application. It is a display of local truth about a
remote claim. It also plays directly to the existing strength: we already
canonicalize and hash everything (RFC 8785, §14), so verifying a reference is
a natural extension of machinery that exists.

I think this is the most **differentiating** feature in this list. Nobody else
in the agent-messaging space is doing it, and the benchmark says it is the
biggest single failure mode.

### 5.4 Delivery timing — deliver when the recipient can actually receive

**Evidence:** CooperBench found messages that "arrive too late to inform
decisions" and channels flooded with "repetitive status updates," and found
that spending 20% of budget on communication did not improve outcomes. The
approval-fatigue literature is blunter: "Fifty prompts and a tired developer
will beat any wording you pick, every time."

We already coalesce ordinary notifications and never steal focus (§23.3). The
next step uses `agent.wait` and `pane.agent_status_changed`: an approved
delivery to a *working* agent can be held until that agent is genuinely
blocked or idle, rather than interrupting mid-flow. The human still approves;
what changes is *when the approved content lands*.

Call it what it is in the UI — `queued for alice (working) · delivers when idle`
— because a delivery a person approved and cannot see is otherwise alarming.

### 5.5 Unanswered-question tracking

**Evidence:** 26% of CooperBench failures are questions going unanswered,
"breaking decision loops."

We have threads and we have receipts. We do not have a view of *questions I
asked that nobody answered* or *questions asked of me that I have not answered*.
That is a `d` filter in the inbox and a count on the health line. Very cheap,
and it addresses a measured failure mode directly.

Pairs with §35's OQ-007 (should read receipts default on) — the answer is
easier once "unanswered" is a first-class state rather than an inference.

### 5.6 A2A interoperability — a position, not necessarily code

**Evidence:** A2A went v1.0.0 in January 2026 (production-ready) and v1.0.1 in
May 2026, adding an extension mechanism for new data, requirements, RPC
methods and state machines. By February 2026 over 100 enterprises had joined,
and the three-layer stack (MCP for tools, A2A for agents, WebMCP for web) is
described as consensus architecture.

A2A's `Task` lifecycle — `submitted`, `working`, `input-required`, `completed`,
`canceled`, `failed` — is close enough to §18.4's delegation states that the
mapping is nearly mechanical. Its **Agent Card** is a discovery document, which
is a different shape from our signed roster but serves an overlapping purpose.

I would **not** adopt A2A as a transport. It assumes reachable endpoints and
online negotiation; HRC's whole point is that both parties may be offline and
that the transport carries opaque encrypted objects (§5, principle 4). But
§21's transport adapter interface and §30's M5 "provider ecosystem" are exactly
where an A2A adapter belongs, and stating the relationship in the PRD is worth
doing before someone asks. The honest framing: **A2A is how agents talk when
they can reach each other; HRC is how they talk when they cannot, and when
their owners are different people.**

There is also a defensive reason to engage. A2A v1.0.1's extension mechanism is
the natural place someone would bolt on the consent gate we have already built.
Better to be the reference for that than to be routed around.

### 5.7 Shared plan object (speculative, M5+)

Appleton's diagnosis is that the planning phase collapsed and coordination got
pushed to the PR. Her prototype's answer is an editable spec both sides see
before agents build. §20 context packages are a one-shot push; a plan is a
*mutable shared object*, which our append-only, no-shared-memory model
deliberately excludes (§5: "shared agent memory").

I flag it because it is clearly where the market is going, and note that it
conflicts with a stated non-goal. That is a product-owner call, not an
implementation one. If it is ever taken, the append-only history makes a
CRDT-ish "plan as a sequence of signed amendments" the shape that fits — not a
shared mutable document.

---

## 6. UI ideas

### 6.1 What the current design already gets right

Measured against the 2026 TUI guidance — spatial consistency, keyboard
fluency, information density, "answer *does anything need me?* first" — the
inbox is in good shape. Fixed panel position, columns dropped whole rather than
squeezed, marks spelled rather than coloured, health line answering the first
question. The dashboard spec for `herdr-agentflow` independently arrived at the
same two-question hierarchy. Keep it.

### 6.2 The weakest thing in the product

**The sender column shows a truncated principal** (`PpWNIUyib…`). Everything
else in the design is carefully human-readable and then the most important
column on every row is base64.

There is no local alias store, and §23.2 is right that nothing a sender chose
may reach the pane. But a **locally assigned** name is not sender-chosen. The
safety phrase ceremony in §15 is the exact moment a human looks at a person and
confirms who they are — that is where you capture "this is Alice," locally,
under the receiver's control. Store it beside the principal; render it; fall
back to the truncated key when absent; never let a remote party influence it.

This is the highest-impact UI change available and it is small.

### 6.3 Ambient indicator, properly

§23.1 concluded that Herdr has no sidebar surface for a plugin. That is true of
a *plugin-owned* surface, but two host surfaces will carry our state:

- `client.window_title.set` → `⚑2 · hrc` in the OS window title. Visible when
  the terminal is not focused, which is exactly when a waiting message matters.
- `pane.report_metadata --token` → a token beside the inbox pane in Herdr's own
  Agent sidebar.

Neither can carry a halt reason, so DEC-096 stands for the sentence. Both can
carry a count. Recommend reopening OQ-015 as "which surface carries which
fact" rather than "which surface wins."

### 6.4 Thread view as a `zoomed` pane

Five of our seven panes are 80%×80% popups. A conversation is the one thing
that wants the whole screen and wants to stay open. `placement = "zoomed"` is
unused and is the right home for a thread reader — with, per §23.2's boundary,
no decision reachable from it.

### 6.5 Make the quarantine legible, not just enforced

The 2026 prompt-injection guidance (Five Eyes joint guidance, May 2026;
Microsoft's August 2026 zero-trust guidance) converges on treating retrieved
content as hostile by default through **sanitization, labeling, and
constraining what content can influence action policies**. Data marking —
delimiting untrusted input with explicit markers — is described as a
floor-level defence.

We do all of this in code (§19, HRC-SEC-009) and almost none of it *visibly*.
The review popup should say, in the frame, that what is inside came from
another machine and is data. Not a warning dialog people click through — a
permanent, structural part of the frame. Cursor's `.cursor/mcp.json` incident
and the GitHub MCP cross-repo exfiltration both happened to people who would
have said they knew the risk.

This is also the screenshot that sells the product. It is the one thing in the
marketplace cluster nobody else can show.

### 6.6 Small things

- **Empty state.** A new channel's inbox says nothing is there. It should say
  what to do next — the single most common moment a new user gives up.
- **Age formatting.** `12m`, `2h14m`, `3d` (agentflow's convention) rather than
  seconds. Already close.
- **Dim for reference data.** Sixteen ANSI colours carrying meaning only;
  everything else DIM. §27 prefers words over colour for *meaning* — using dim
  for *recession* does not conflict with that.

---

## 7. What I would do next, in order

| # | Item | Effort | Why first |
|---|---|---|---|
| 1 | Add the `herdr-plugin` GitHub topic (+ `herdr`, `agents`, `e2e-encryption`) | minutes | invisible in a 1,218-plugin marketplace |
| 2 | Rewrite the manifest `description` to say *two people*, *end-to-end encrypted*, *not remote control* | minutes | we are being read as a phone dashboard |
| 3 | Local alias store, captured at the safety-phrase ceremony (§6.2) | small | the worst thing in the UI |
| 4 | `client.window_title.set` ambient indicator (§6.3) | small | the indicator §23.1 wanted |
| 5 | Unanswered-question state and filter (§5.5) | small | addresses a measured 26% failure mode |
| 6 | `link_handlers` + QR invite (§5.2) | medium | closes the worst usability gap |
| 7 | Verifiable result references (§5.3) | medium | the differentiator; a measured 32% failure mode |
| 8 | `pane.agent_status_changed` → blocked-agent broadcast (§5.1) | medium | the most-wanted thing in the ecosystem |
| 9 | Close DEC-091 using `server.agent_manifests` + `agent.start` (§4.3) | medium | no longer blocked on a missing capability |
| 10 | State the A2A relationship in §21/M5 (§5.6) | writing | before someone asks |

Items 1 and 2 are not engineering. They are the reason nobody has found this
product.

---

## 8. Open questions this raises for the product owner

- **OQ-015 should reopen** as "which surface carries which fact" (§6.3).
  DEC-096 remains correct for the halt sentence.
- **Is blocked-agent broadcast (§5.1) presence?** §5 lists real-time presence
  as a non-goal. I read it as an event, not presence, but that is a call.
- **Does a shared plan object (§5.7) violate "shared agent memory"?** I think
  yes as stated, and that the non-goal may be what needs revisiting rather than
  the feature.
- **Should the product be renamed** to escape the `herdr-remote-*` cluster
  (§3)? Costly, and the npm package and plugin id are already published.
  Probably answered by the description rather than the name.

---

## Sources

- CooperBench — [paper](https://arxiv.org/html/2601.13295v1) · [site](https://cooperbench.com/) · [repo](https://github.com/cooperbench/CooperBench) · [OpenReview](https://openreview.net/forum?id=AomNqiSwb1)
- Maggie Appleton, *One Developer, Two Dozen Agents, Zero Alignment* — https://maggieappleton.com/zero-alignment
- Herdr plugin docs — https://herdr.dev/docs/plugins/
- Herdr socket API — https://herdr.dev/docs/socket-api/
- Herdr marketplace — https://herdr.dev/plugins/ · https://herdr.dev/docs/marketplace/
- Herdr on Hacker News — https://news.ycombinator.com/item?id=48714802
- `herdr-plugin` GitHub topic — https://github.com/topics/herdr-plugin
- herdr-remote — https://github.com/dcolinmorgan/herdr-remote · herdr-push — https://github.com/dcolinmorgan/herdr-push
- herdr-mobile-relay — https://github.com/0cv/herdr-mobile-relay · herdr-web — https://github.com/barnuri/herdr-web
- herdr-ntfysh — https://github.com/cobanov/herdr-ntfysh · cmux-herdr — https://github.com/tomoya55/cmux-herdr
- herdr-agentflow dashboard spec — https://github.com/ogglord/herdr-agentflow/issues/15
- A2A protocol — https://a2a-protocol.org/latest/ · https://en.wikipedia.org/wiki/Agent2Agent
- LangChain Agent Inbox — https://github.com/langchain-ai/agent-inbox · [interrupt](https://www.langchain.com/blog/making-it-easier-to-build-human-in-the-loop-agents-with-interrupt)
- Microsoft, *Advance Zero Trust for AI* (Aug 2026) — https://www.microsoft.com/en-us/security/blog/2026/08/04/advance-zero-trust-for-ai-new-tools-and-guidance-to-secure-ai-agents-and-devsecops/
- Prompt injection landscape — https://www.sysdig.com/learn-cloud-native/prompt-injection
- Approval fatigue — https://workos.com/blog/approval-fatigue-agent-governance · https://aipatternbook.com/approval-fatigue
- TUI design guidance — https://hyperbliss.tech/blog/2026.04.04_terminal-renaissance/
- Multiplayer coding agents — https://aq.dev/multiplayer-coding-agents/ · https://www.tembo.io/blog/multi-agent-ai-coding-workflows
