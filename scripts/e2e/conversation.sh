#!/usr/bin/env bash
#
# Two installations, one repository, a whole conversation.
#
# Everything here runs against an `hrc` on `PATH` -- the published one in CI --
# rather than a build tree, because the thing worth testing is what a person
# installs. The unit and contract suites already cover the protocol against a
# locally built binary; what they cannot tell us is whether the artifact we
# shipped works.
#
# Alice and Bob are separate `HRC_HOME` directories: separate key stores,
# separate databases, no shared private material. That is two installations in
# every sense the protocol cares about, and unlike two CI jobs it does not
# require publishing an invite secret through an artifact that anyone can
# download.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'cleanup' EXIT

alice="$work/alice"
bob="$work/bob"
channel="$work/channel.git"
daemons=()

cleanup() {
    for pid in ${daemons+"${daemons[@]}"}; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    rm -rf "$work"
}

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
ok()   { printf '   ok  %s\n' "$*"; }
die()  { printf '\n\033[31mFAILED: %s\033[0m\n' "$*" >&2; exit 1; }

# Every command runs as one of the two installations. `HRC_PASSPHRASE` is set
# per invocation rather than exported once, so neither home can accidentally
# open the other's key store.
as() {
    local home="$1"; shift
    HRC_HOME="$home" HRC_PASSPHRASE="passphrase-for-$(basename "$home")" hrc "$@"
}

json() { python3 -c 'import json,sys;print(json.load(sys.stdin)'"$1"')'; }

# Starts a daemon and returns once its socket answers, so a caller never races
# the announcement.
start_daemon() {
    local home="$1"
    HRC_HOME="$home" HRC_PASSPHRASE="passphrase-for-$(basename "$home")" \
        hrc daemon --json >"$home/daemon.log" 2>&1 &
    daemons+=("$!")

    for _ in $(seq 1 100); do
        [ -S "$home/run/hrc-trusted.sock" ] && return 0
        sleep 0.1
    done
    cat "$home/daemon.log" >&2 || true
    die "the daemon for $(basename "$home") never opened its trusted socket"
}

stop_daemons() {
    for pid in ${daemons+"${daemons[@]}"}; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    daemons=()
}

trusted() {
    python3 "$here/trusted-call.py" "$1/run/hrc-trusted.sock" "$2" "$3"
}

step "the published binary answers"
hrc --version || die "hrc is not on PATH"
ok "$(hrc --version)"

step "one repository backs the channel"
git init --quiet --bare "$channel"
ok "$channel"

step "two installations initialize independently"
as "$alice" init --json >/dev/null || die "alice could not initialize"
as "$bob"   init --json >/dev/null || die "bob could not initialize"
alice_id="$(as "$alice" whoami --json | json '["signingKey"]')"
bob_id="$(as "$bob" whoami --json | json '["signingKey"]')"
if [ -z "$alice_id" ] || [ -z "$bob_id" ]; then
    die "an installation has no principal"
fi
[ "$alice_id" != "$bob_id" ] || die "both installations share a principal"
ok "alice $alice_id"
ok "bob   $bob_id"

step "doctor passes on a fresh installation"
as "$alice" doctor --json >/dev/null || die "doctor failed after init"
ok "every check passed"

step "alice creates the channel"
as "$alice" create --repo "$channel" --json >/dev/null || die "create failed"
channel_id="$(as "$alice" channels --json | json '["channels"][0]["channelId"]')"
alice_principal="$(as "$alice" members --json | json '["members"][0]["principalId"]')"
ok "channel $channel_id"
ok "alice is $alice_principal in the roster"

# The three checks below are written as `if ...; then die; fi` rather than
# `... && die`. Both work, but the second leans on which command in a `&&`
# list `set -e` exempts, and these are the assertions that a secret did not
# leak. An assertion that quietly stops asserting is worse than no assertion,
# so they say what they mean.
step "alice invites bob, and the code is shown once"
invite_output="$(as "$alice" invite create --github-user bob-e2e 2>&1)" \
    || die "invite create failed: $invite_output"
invite_code="$(printf '%s' "$invite_output" | grep -oE '[A-Za-z0-9_.:-]{24,}' | head -1)"
[ -n "$invite_code" ] || die "no invite code in: $invite_output"
ok "an invite was issued"

# The secret must not be recoverable from anything an agent can read.
if as "$alice" invite list --json | grep -q "$invite_code"; then
    die "invite list disclosed the secret"
fi
ok "invite list does not carry the code"

step "bob joins with the code"
as "$bob" join "$invite_code" --json >/dev/null || die "join failed"
ok "a request was published"

step "alice sees the request pending"
as "$alice" sync --once --json >/dev/null || die "alice could not synchronize"
pending="$(as "$alice" join pending --json)"
request_id="$(printf '%s' "$pending" | json '["pending"][0]["requestId"]')"
[ -n "$request_id" ] || die "no pending request: $pending"
phrase_alice="$(printf '%s' "$pending" | json '["pending"][0]["safetyPhrase"]')"
[ -n "$phrase_alice" ] || die "no safety phrase for the human to compare"
ok "request $request_id"
ok "safety phrase present"

step "the agent-safe path cannot admit anyone"
start_daemon "$alice"
if python3 "$here/trusted-call.py" "$alice/run/hrc-agent.sock" approve_join \
        "{\"request_id\": \"$request_id\"}" >/dev/null 2>&1; then
    die "the agent-safe socket admitted a member"
fi
ok "refused, as section 22.7 requires"

step "alice admits bob through the trusted interface"
trusted "$alice" approve_join "{\"request_id\": \"$request_id\"}" >/dev/null \
    || die "approve_join failed"
stop_daemons
members="$(as "$alice" members --json)"
count="$(printf '%s' "$members" | json '["members"].__len__()')"
epoch="$(printf '%s' "$members" | json '["rosterEpoch"]')"
[ "$count" = "2" ] || die "expected two members, got $count: $members"
[ "$epoch" = "1" ] || die "admitting a member must advance the epoch, got $epoch"
ok "two members, roster epoch $epoch"

# Address whoever the roster says is there, rather than an identifier read
# from the other installation. A sender only ever knows the roster, and the
# two are not the same string.
bob_principal="$(printf '%s' "$members" | python3 -c '
import json, sys
roster = json.load(sys.stdin)["members"]
mine = sys.argv[1]
print(next(m["principalId"] for m in roster if m["principalId"] != mine))
' "$alice_principal")"
[ -n "$bob_principal" ] || die "the roster names no second principal"
ok "bob is addressable as $bob_principal"

step "the invite is spent"
state="$(as "$alice" invite list --json | json '["invites"][0]["state"]')"
[ "$state" = "consumed" ] || die "invite state is $state"
ok "consumed"

note="the whole product, end to end"
step "alice sends bob a note"
as "$alice" sync --once --json >/dev/null
send_output="$(as "$alice" send "$bob_principal" "$note" --json 2>&1)" \
    || die "send failed: $send_output"
as "$alice" sync --once --json >/dev/null || die "publishing failed"
ok "published"

step "the plaintext is not in the repository"
if git --git-dir="$channel" grep -q "$note" \
        "$(git --git-dir="$channel" rev-parse hrc)" 2>/dev/null; then
    die "the plaintext reached the published branch"
fi
ok "only ciphertext was published"

step "bob receives it quarantined, with the body withheld"
as "$bob" sync --once --json >/dev/null || die "bob could not synchronize"
inbox="$(as "$bob" inbox --json)"
message_id="$(printf '%s' "$inbox" | json '["entries"][0]["messageId"]')"
[ -n "$message_id" ] || die "nothing arrived: $inbox"
if printf '%s' "$inbox" | grep -q "$note"; then
    die "the inbox disclosed a body nobody approved"
fi
ok "message $message_id is present and its body is not"

step "the Herdr inbox pane offers the row a person would select"
# The pane a person keeps open is interactive on a terminal and the same rows
# as JSON otherwise, so this drives the half a script can reach: that the row
# carries the identifier the side view selects and targets review by, that it
# reports a disposition, and that neither the rows nor the notifications
# disclose anything nobody approved.
pane="$(as "$bob" herdr pane inbox --json)"
pane_id="$(printf '%s' "$pane" | json '["rows"][0]["message_id"]')"
[ "$pane_id" = "$message_id" ] \
    || die "the pane row does not name the message it is about: $pane"
pane_state="$(printf '%s' "$pane" | json '["rows"][0]["disposition"]')"
[ "$pane_state" = "pending" ] \
    || die "a quarantined message should read as pending, not $pane_state"
if printf '%s' "$pane" | grep -q "$note"; then
    die "the Herdr inbox pane disclosed a body nobody approved"
fi
ok "the pane row targets $pane_id and is $pane_state"

step "a note is not announced, and a question is announced once"
# Section 23.3 lists what notifies. A note is not on that list: it is not
# urgent, and interrupting someone over one is the behaviour that makes people
# turn notifications off. A question is on the list.
#
# The second read is the point of the durable ledger. The side view reloads
# once a second and a plugin pane process lives for one render, so "already
# announced" has to survive both — reading the pane twice must announce a
# message once.
quiet="$(printf '%s' "$pane" | json '["notifications"]')"
[ "$quiet" = "[]" ] || die "a note should not notify: $quiet"

question="does the notification fire exactly once"
as "$alice" ask "$bob_principal" "$question" --json >/dev/null || die "ask failed"
as "$alice" sync --once --json >/dev/null
as "$bob" sync --once --json >/dev/null

announced="$(as "$bob" herdr pane inbox --json | json '["notifications"]')"
[ "$announced" != "[]" ] || die "a question raised nothing: $announced"
if printf '%s' "$announced" | grep -q "$question"; then
    die "a notification carried the message text"
fi

again="$(as "$bob" herdr pane inbox --json | json '["notifications"]')"
[ "$again" = "[]" ] \
    || die "reading the pane a second time announced the same message again: $again"
ok "announced once: $announced"

step "an agent cannot read the body before a human releases it"
start_daemon "$bob"
if python3 "$here/trusted-call.py" "$bob/run/hrc-agent.sock" show_approved \
        "{\"message_id\": \"$message_id\"}" 2>/dev/null | grep -q "$note"; then
    die "the agent-safe surface disclosed a body nobody approved"
fi
ok "the agent-safe surface has nothing to give"

step "bob approves it through the trusted interface"
expires="$(python3 -c 'import datetime;print((datetime.datetime.now(datetime.UTC)+datetime.timedelta(minutes=5)).strftime("%Y-%m-%dT%H:%M:%SZ"))')"
trusted "$bob" preview_pending "{\"message_id\": \"$message_id\"}" >/dev/null \
    || die "preview_pending failed"

# A decision is a tagged object, not a word, and which one matters. The human
# chooses what happens to the content: `deliver_to_agent` releases it to the
# agent they named, which is the whole point of the prompt gate, while
# `keep_in_inbox` leaves it quarantined for a person to handle by hand.
trusted "$bob" approve \
    "{\"message_id\": \"$message_id\", \"decision\": {\"action\": \"deliver_to_agent\", \"agent\": \"reviewer\"}, \"expires_at\": \"$expires\"}" \
    >/dev/null || die "approve failed"
ok "approved, and released to the agent the human named"

step "now the agent can read exactly what was released"
released="$(python3 "$here/trusted-call.py" "$bob/run/hrc-agent.sock" show_approved \
    "{\"message_id\": \"$message_id\"}")" || die "show_approved failed: $released"
printf '%s' "$released" | grep -q "$note" \
    || die "approved content is not readable on the agent-safe surface: $released"
stop_daemons
ok "the prompt gate opened once, for one message"

step "bob replies, and alice reads it in the same thread"
as "$bob" reply "$message_id" "received, end to end" --json >/dev/null \
    || die "reply failed"
as "$bob" sync --once --json >/dev/null
as "$alice" sync --once --json >/dev/null
reply_inbox="$(as "$alice" inbox --json)"
reply_id="$(printf '%s' "$reply_inbox" | json '["entries"][0]["messageId"]')"
[ -n "$reply_id" ] || die "the reply never arrived: $reply_inbox"
ok "reply $reply_id arrived"

step "alice's message reached bob, and she can see that"
as "$alice" sync --once --json >/dev/null
shown="$(as "$alice" show "$message_id" --json)"
printf '%s' "$shown" | grep -qE 'delivered|published' \
    || die "no delivery state for a message that plainly arrived: $shown"
ok "$(printf '%s' "$shown" | json '.get("state", "state reported")')"

step "alice revokes bob's device, and the epoch advances"
# `AC-REVOCATION` end to end, which until now was proven only in unit tests
# against roster structures rather than over a channel two installations were
# actually using.
bob_device="$(as "$alice" members --json | python3 -c '
import json, sys
roster = json.load(sys.stdin)["members"]
mine = sys.argv[1]
member = next(m for m in roster if m["principalId"] != mine)
print(member["devices"][0]["deviceId"])
' "$alice_principal")"
[ -n "$bob_device" ] || die "bob has no device to revoke"

start_daemon "$alice"
trusted "$alice" revoke_device "{\"device_id\": \"$bob_device\"}" >/dev/null \
    || die "revoke_device failed"
stop_daemons

after="$(as "$alice" members --json)"
epoch_after="$(printf '%s' "$after" | json '["rosterEpoch"]')"
[ "$epoch_after" = "2" ] \
    || die "revocation must advance the epoch, got $epoch_after: $after"
active="$(printf '%s' "$after" | python3 -c '
import json, sys
roster = json.load(sys.stdin)["members"]
member = next(m for m in roster if m["principalId"] != sys.argv[1])
print(any(d["active"] for d in member["devices"]))
' "$alice_principal")"
[ "$active" = "False" ] || die "the revoked device is still active: $after"
ok "device $bob_device revoked, roster epoch $epoch_after"

step "nothing new reaches the revoked device"
as "$alice" sync --once --json >/dev/null || die "alice could not publish the revocation"
before_count="$(as "$bob" inbox --json | json '["entries"].__len__()')"
as "$alice" send "$bob_principal" "after the revocation" --json >/dev/null 2>&1 || true
as "$alice" sync --once --json >/dev/null 2>&1 || true
as "$bob" sync --once --json >/dev/null 2>&1 || true
after_count="$(as "$bob" inbox --json | json '["entries"].__len__()')"
[ "$after_count" = "$before_count" ] \
    || die "a revoked device received $((after_count - before_count)) new message(s)"
if as "$bob" inbox --json | grep -q "after the revocation"; then
    die "a revoked device read content published after its revocation"
fi
ok "the revoked device gained nothing"

step "the boundary holds for an agent"
for refused in review approve rollover; do
    if as "$alice" "$refused" --json >/dev/null 2>&1; then
        die "\`hrc $refused\` did not refuse a non-interactive caller"
    fi
done
ok "review, approve and rollover all refuse"

printf '\n\033[32mEnd to end: the whole conversation completed.\033[0m\n'
