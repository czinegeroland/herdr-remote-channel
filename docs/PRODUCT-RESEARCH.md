# Product research: what to build next

*September 2026. A product-design pass over the web research, set against what
Herdr Remote Channel already does. It follows `docs/RESEARCH.md`, whose ideas
have now all been built or decided (DEC-104 to DEC-115).*

## 1. The short version

The product is sound where it is strict: nothing reaches an agent unless a
person released it, and the security research of 2026 keeps proving that
strictness right. Where it is weak is **what the strictness costs a person
every day**. There are three weaknesses, and they have one root cause:

1. **Every message costs the same.** A one-line "thanks" and a 200-line task
   go through the same reveal, propose, and confirm steps. Approval-fatigue
   research is blunt about where that leads: reviewers start approving
   reflexively, and at that point the gate protects nothing.
2. **You can't see the work, only the messages.** Nothing in the product
   answers "what did I ask Alice to do, and where is it?", even though every
   fact needed for that answer is already in local metadata.
3. **Approved content lands in the agent as if you had typed it.** That is
   exactly the escalation path the new research names.

The root cause is that the gate is binary. The recommendation (§3.1) is to
**make the gate proportional rather than weaker**. That is also the safe form
of the "auto mode" the product owner asked for.

## 2. What the research says

### 2.1 Approval fatigue is a security problem, not a UX nit

- Production guidance now classifies agent actions into four risk tiers
  (read-only, reversible, external, irreversible) and puts a human only on
  the tiers that need one. Gating read-only work "only manufactures
  confirmation fatigue".
- "Confirmation fatigue is not just bad UX; it is a documented clickthrough
  vulnerability." The warning sign is measurable: approvals get faster and
  the edit rate falls toward zero.
- Good approvals are not yes/no. They offer "reject with edits", carry a
  plain-language description and a diff, and show an approval deadline.
- Once an action's measured error rate stays under a threshold (about 5% is
  typical), it is promoted from "in the loop" to "on the loop". That is
  graduated autonomy, earned per action rather than switched on globally.

### 2.2 Inter-agent content is the attack surface of 2026

- *Comment and Control* (April 2026): one payload in a pull-request comment
  made three vendors' coding agents leak secrets. The content came from
  another party and was processed as trusted.
- OWASP's agentic Top 10 now lists **ASI07 Insecure Inter-Agent
  Communication** as its own risk.
- *Context privilege escalation* (arXiv 2609.01222) names the exact mechanism
  to avoid: content from a low-privilege source ends up in a higher-privilege
  message role. **An approved remote message typed into an agent as a user
  prompt is the textbook case.** The provenance banner helps, but the content
  still arrives in the role the agent trusts most.

### 2.3 Handoffs fail when context is lost

- The most requested Claude Code team feature is a portable session handoff
  (issue #69224, closed as not planned). Its use case is our product's:
  "one developer investigates… and wants to hand it to the implementing
  developer with full context."
- Handoff write-ups agree on what a real handoff carries: the diffs, the
  branch and worktree state, the decisions made, and the open questions. A
  transcript or a link "forces teammates to… hope the model reconstructs the
  context"; "if they have to re-explain the task, the handoff already
  failed."
- Developers now spend more time reviewing AI output than writing code
  (11.4 vs 9.8 hours a week). A handoff lands on a reviewer, not a typist.

### 2.4 Noise management has settled patterns

- Microsoft's Teams guidance for agents in shared spaces: acknowledge with a
  reaction instead of a message, reply in threads, and quote the message
  being answered when replying late. The rule is that the best agents
  "know when and how to answer" and show restraint.
- Multi-agent research (arXiv 2609.19759, *When More Is Less*):
  collaboration helps on long-horizon tasks with sparse dependencies and
  hurts on tightly coupled ones. More agents and more messages are not more
  intelligence.

### 2.5 Verification rituals get skipped

- Signal shipped Automatic Key Verification in August 2026 because "almost
  nobody" compared safety numbers by hand. Enrolment here depends on
  comparing a safety phrase. It is probably skipped just as often, and the
  product cannot tell.

### 2.6 The ecosystem

- The Herdr marketplace has about 1,300 plugins. The popular ones are
  workspace tools (projects, review sidebars, live session graphs). None
  connects two people. The niche is still empty, as §3 of the first research
  found.
- Mobile approval tools (Pushary, AgentField, Agent Approve) are a new
  category: agents ask, and a phone answers. They approve *tool calls on your
  own machine*. Nobody does it for *content from another person*, and a
  phone-side approval for that would move the trusted interface off the
  machine.

## 3. Recommendations, ranked

Ranked by (evidence) × (fit with the gate and the §5 non-goals) ÷ effort.
Each gives the problem, the design, and what it would take.

### 3.1 A proportional gate: trust tiers per member and per kind

**Problem.** A binary gate either fatigues people (§2.1) or, as "auto mode",
switches protection off entirely. Neither is what a person wants from a
long-standing collaborator.

**Design.**
- The person, and only on the trusted screen, sets a **trust level** for each
  member: *review everything* (today's default), *trusted for
  conversation*, or *trusted for tasks*.
- A trust level releases only low-risk kinds from a trusted member into a
  **mailbox**, never into an agent prompt (see §3.2). Examples: notes,
  answers to a question this installation asked, and task accept, decline
  and progress.
- Context packages, anything carrying the prompt capability, and anything
  from an untrusted member keep the full review.
- Each member's trust is local, revocable with one key, and expires by
  default (for example after 7 days, renewable). A roster change drops the
  new principal to *review everything*.
- The audit log records each automatic release with the rule that allowed it.
- A *rubber-stamp detector* watches for consecutive approvals made faster than
  reading speed. When it fires it says so, and offers either a trust level
  (if the person really trusts this member) or batching (§3.4).

**Why this and not "auto mode for all".** It gives the product owner's ask,
agents working together without a click each time, for the traffic that is
safe to automate. It keeps the gate meaningful for the traffic that isn't.
It is also local: nobody's consent changes what *my* agent receives, so the
design questions about everyone opting in go away.

**Effort.** Large. It needs a trust-level store (migration), a decision path
for automatic releases recorded as a new decision kind, the mailbox (§3.2),
screen changes, and a PRD change to HRC-GATE-007. That change needs the
product owner's explicit decision.

### 3.2 Deliver approved content as data, not as a prompt

**Problem.** `agent.prompt` puts released remote content in the user role
(§2.2).

**Design.**
- Add a delivery mode that writes the approved content, with its provenance
  banner, to a file in the destination agent's working directory, for
  example `.hrc/inbox/<id>.md`.
- The agent is then prompted with a fixed local sentence that names the file
  and says it is data from another person. The prompt carries no remote text.
- The agent reads the file as a tool result, which is the role agents already
  treat as untrusted.
- This becomes the default. Typing into the prompt stays available as an
  explicit choice.

**Effort.** Small to medium. It is a second delivery function beside
`hand_to_herdr`, plus a config setting and a §23.4 change. It is also what
makes §3.1's mailbox safe.

### 3.3 A delegations board: what I asked for, and where it is

**Problem.** Tasks, acceptances, progress and results are all in local
metadata (kinds, threads, receipts), and nothing shows them together
(§2.3, §2.4).

**Design.**
- A pane listing every task this installation sent or received, with columns
  for who, the lifecycle state derived from message kinds (requested,
  accepted, in progress, blocked, result, declined), age, and whether a
  result named a commit that resolves here.
- `Enter` opens the thread.
- It is metadata only, the same closed set the inbox shows, so it is safe on
  screen.

**Effort.** Medium. The state is derived from existing kinds, with no
protocol change.

### 3.4 Batch review and edit-before-deliver

**Problem.** Reviewing one message at a time is the fatigue path. Rejecting
with edits is the recommended alternative to yes/no, and the core already
supports it (`Decision::DeliverEdited`), but the screen offers no way to edit.

**Design.**
- `e` on a revealed body opens it in an editor, and the edited text is what
  gets delivered. The decision records both the original and the edit, as
  the gate already requires.
- A batch mode: select several messages from one sender, read each (reveal
  stays per message), then confirm the set in one step.

**Effort.** Medium for edit (the TUI plus wiring `DeliverEdited` through
`WireDecision`), small for batch.

### 3.5 Handoff as a first-class message

**Problem.** A delegation carries a description. A real handoff carries the
diffs, the branch, what was decided, and what is still open (§2.3).

**Design.**
- `hrc handoff <recipient>` builds a task from:
  - the current branch and its commits (as `commit` references, already
    checkable, DEC-105)
  - a `branch` reference kind (the name plus the tip it pointed at)
  - an optional context package
  - a structured body with three sections: *what I found*, *what I decided*,
    and *what is open*
- The receiver's review shows the references checked against their checkout,
  and a `git fetch` of the branch stays their own action.

**Effort.** Medium. It needs a new reference kind, a command, and a skill
section. It builds directly on #96 and #104.

### 3.6 "While you were away"

**Problem.** An async product needs a way back in (§2.3).

**Design.**
- When the inbox gains focus after an absence (a Herdr focus event after
  more than 30 minutes), its header summarizes what happened in counts only.
  For example: `Since 14:05 — 2 answers, 1 result, 1 task waiting on you, 1
  question you owe`.
- Counts and locally resolved names only, so it is safe on the agent-safe
  surface.

**Effort.** Small. Every count already exists (unanswered from DEC-103, plus
kinds and receipts).

### 3.7 Lightweight acknowledgements

**Problem.** Replies such as "on it" and "thanks" are messages, and every
message costs a review (§2.4).

**Design.**
- An `ack` kind whose body is one value from a closed set (seen, on it,
  done, thanks) and never free text, so there is nothing to quarantine. It
  shows as a mark on the message it acknowledges rather than as a row.
- `task accept` could render the same way.

**Effort.** Small to medium. It is a closed-body kind, and the §18.2 table
gains a row.

### 3.8 Make verification real, and visible when it lapses

**Problem.** Safety-phrase comparison is probably skipped (§2.5), and a
changed device key looks like any other roster change.

**Design.**
- Record per member whether the phrase was confirmed, and show `unverified`
  on the members screen and in the review header.
- Treat a new device for an already-verified principal as a visible event
  ("Alice added a device") that re-asks for confirmation.
- Longer term, the channel's append-only control log is already a
  transparency log. A second member witnessing it is the key-transparency
  answer Signal chose.

**Effort.** Small for the badge and the device-change prompt. Large for
witnessing.

### 3.9 An audit pane

**Problem.** Trust in a gate comes from seeing what it did. `hrc audit`
exists only as a command.

**Design.** A read-only pane listing approvals, declines, and automatic
releases (once §3.1 exists), each with who decided, when, and which agent
received it.

**Effort.** Small.

### 3.10 Not recommended

- **Phone-side approval of remote content.** It moves the trusted interface
  off the machine that holds the keys and the agent (§2.6). A phone
  *notification* carrying metadata only is fine, and Herdr's own notifications
  plus existing notify plugins already cover it.
- **Real-time presence or "live" shared sessions.** Still a §5 non-goal
  (DEC-112). The delegations board (§3.3) answers "what is happening" from
  things people chose to send.
- **More agents per task.** §2.4: collaboration pays on sparse,
  long-horizon work. The product should make one well-specified delegation
  cheap, not many.

## 4. Suggested order

| # | Item | Effort | Why now |
|---|---|---|---|
| 1 | Deliver as data, not as a prompt (§3.2) | small–medium | closes the escalation path the 2026 research names, and every later item depends on it |
| 2 | "While you were away" (§3.6) | small | cheap, all data exists, the biggest daily-use gain |
| 3 | Delegations board (§3.3) | medium | makes the delegation features shipped this month usable as a workflow |
| 4 | Edit-before-deliver and batch review (§3.4) | medium | the core already supports editing, and it is the standard answer to approval fatigue |
| 5 | Verification badge and device-change prompt (§3.8) | small | cheap, and closes a quiet gap |
| 6 | Audit pane (§3.9) | small | trust through visibility |
| 7 | Handoff (§3.5) | medium | the most-requested workflow in the ecosystem |
| 8 | Acknowledgements (§3.7) | small–medium | less noise |
| 9 | Proportional gate / trust tiers (§3.1) | large | the safe form of auto mode, after items 1 and 4 |

Item 9 replaces the open auto-mode design questions. It is still the product
owner's decision, because it changes HRC-GATE-007.

## Sources

- [Human-in-the-Loop Escalation Design for AI Agents 2026 — Digital Applied](https://www.digitalapplied.com/blog/human-in-the-loop-escalation-design-ai-agents-2026)
- [Human-in-the-Loop Patterns for AI Agents (2026) — MyEngineeringPath](https://myengineeringpath.dev/genai-engineer/human-in-the-loop/)
- [Human-in-the-loop patterns for AI agents in Jira — Atlassian](https://www.atlassian.com/software/jira/guides/agentic-engineering/human-in-the-loop)
- [Human-in-the-Loop AI Agents: The 2026 Guide — Pickaxe](https://pickaxe.co/post/human-in-the-loop-ai-agents)
- [Building Agents for Teams: Managing the noise of collaboration — Microsoft 365 Developer Blog](https://devblogs.microsoft.com/microsoft365dev/building-agents-for-teams-managing-the-noise-of-collaboration/)
- [Rethinking Multi-Agent Collaboration: When More Is Less — arXiv 2609.19759](https://arxiv.org/abs/2609.19759)
- [AgentRoom: Concurrent Multi-Agent Coding in a CRDT-Backed Shared Workspace — arXiv 2608.23740](https://arxiv.org/pdf/2608.23740)
- [What's in Your Agent's Context? Context Privilege Escalation Attacks — arXiv 2609.01222](https://arxiv.org/abs/2609.01222)
- [Three AI coding agents leaked secrets through a single prompt injection — VentureBeat](https://venturebeat.com/security/ai-agent-runtime-security-system-card-audit-comment-and-control-2026)
- [Prompt injection still drives most agentic AI security failures — Help Net Security](https://www.helpnetsecurity.com/2026/06/11/owasp-prompt-injection-ai-security-failures/)
- [AI Coding Agent Security (2026) — Morph](https://www.morphllm.com/ai-coding-agent-security)
- [Team sessions — shared, portable sessions (anthropics/claude-code #69224)](https://github.com/anthropics/claude-code/issues/69224)
- [How to Share a Claude Code Session with Your Team — Lody](https://lody.ai/blog/handoff-claude-code-session/)
- [The best ways to share a Claude Code session (2026) — Lore](https://lore.link/blog/best-ways-to-share-a-claude-code-session)
- [Introducing Automatic Key Verification — Signal](https://signal.org/blog/automatic-key-verification/)
- [How Safe Is Safety Number? A User Study on Signal's Fingerprint and Safety Number Methods](https://gcris.etu.edu.tr/handle/20.500.11851/1957)
- [Herdr plugins marketplace](https://herdr.dev/plugins/) and [A practical guide to Herdr plugins — Flavio Copes](https://flaviocopes.com/herdr-plugins/)
- [Approve your agents from your phone — AgentField](https://agentfield.ai/blog/approve-from-your-phone) and [Pushary](https://pushary.com/manage-ai-agents-remotely)
- [Agent CLI Developer Experience 2026: The 3-Axis DX Test — FutureAGI](https://futureagi.com/blog/agent-cli-developer-experience-2026/)
- [AI Collaboration Tools: Complete Guide for Teams (2026) — Context Link](https://context-link.ai/blog/ai-collaboration-tools)
- [Microsoft Agent Framework at BUILD 2026](https://devblogs.microsoft.com/agent-framework/microsoft-agent-framework-at-build-2026-announce/) and [A2A v1 in Microsoft Agent Framework](https://devblogs.microsoft.com/agent-framework/a2a-v1-is-here-cross-platform-agent-communication-in-microsoft-agent-framework-for-net/)
