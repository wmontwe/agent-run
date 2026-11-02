#!/bin/bash
# Integration checks for bin/agent against the real Podman machine: mounts,
# isolation, network policy, memory, signals and cleanup. Needs a running
# rootless machine and the claude image (podman compose build claude); the
# proxy image builds on first use. Uses scratch repositories and a scratch
# HOME only, never the real ~/.agent-run.
#
#   tests/integration.sh
set -euo pipefail

here="$(cd -P -- "$(dirname -- "$0")" && pwd -P)"
agent="$here/../bin/agent"
scratch="$(cd -P -- "$(mktemp -d "${TMPDIR:-/tmp}/agent-run-it.XXXXXX")" && pwd -P)"
trap 'chmod -R u+rwx "$scratch" 2>/dev/null; rm -rf -- "$scratch"' EXIT

# A scratch HOME hides Podman's connection settings, so pass the rootless one.
connection="$(podman system connection list --format '{{.Name}} {{.URI}} {{.Identity}}' |
    awk '$1 == "podman-machine-default" {print $2, $3}')"
[[ -n "$connection" ]] || { echo "no podman-machine-default connection" >&2; exit 1; }
export CONTAINER_HOST="${connection% *}" CONTAINER_SSHKEY="${connection#* }"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1

passed=0
failed=0
check() {
    if [[ "$2" == "$3" ]]; then
        passed=$((passed + 1)); echo "  ok   $1"
    else
        failed=$((failed + 1)); echo "  FAIL $1: expected '$2', got '$3'"
    fi
}
g() {
    git -c core.hooksPath=/dev/null -c commit.gpgsign=false \
        -c user.name=test -c user.email=test@example.invalid "$@"
}

# fresh NAME: a new workspace and HOME. A fresh path per run: deleting and
# recreating the same path within seconds can leave the VM's share stale.
fresh() {
    S="$scratch/$1"
    mkdir -p "$S/home/.agent-run/knowledge" "$S/home/.agent-run/memory" \
        "$S/home/.agent-run/claude/home" "$S/ws"
    git init -q "$S/ws"
    HOME_DIR="$S/home" WS="$S/ws" MEM="$S/home/.agent-run/memory"
}
run_agent() {
    HOME="$HOME_DIR" WORKSPACE="$WS" /bin/bash "$agent" claude "$@" </dev/null
}
# value KEY: the KEY=... line the probe printed.
value() {
    sed -n "s/^$1=//p" "$S/out" | tr -d '\r' | head -1
}

volumes_before="$(podman volume ls -q | wc -l | tr -d ' ')"

echo "isolation, mounts and network"
fresh probe
mkdir -p "$WS/.idea" "$WS/.signing"
echo idea >"$WS/.idea/a.xml"
echo key >"$WS/.signing/k.jks"
git_config_sum="$(shasum "$WS/.git/config")"
status=0
run_agent bash -c '
    cd /workspace
    w() { if eval "$1" 2>/dev/null; then echo "$2=writable"; else echo "$2=blocked"; fi; }
    w "git config core.fsmonitor x" git_config
    w "touch .git/hooks/x" git_hooks
    w "mv .git .git2" git_mv
    w "echo x > .idea/a.xml" idea_write
    echo "idea_read=$(cat .idea/a.xml)"
    echo "signing_files=$(ls -A .signing | wc -l | tr -d " ")"
    echo "uid_map=$(head -1 /proc/self/uid_map | tr -s " " | sed "s/^ //")"
    echo "cap_eff=$(sed -n "s/^CapEff:\t//p" /proc/self/status)"
    echo "tmp_fs=$(awk "\$2==\"/tmp\"{t=\$3}END{print t}" /proc/mounts)"
    code() { curl -s -o /dev/null -m8 -w "%{http_code}" "$1"; }
    echo "net_allowed=$(code https://api.anthropic.com/)"
    echo "net_denied=$(code https://example.com/)"
    echo "net_private=$(code https://10.0.0.1/)"
    echo "net_http=$(code http://api.anthropic.com/)"
    exit 3' >"$S/out" 2>&1 || status=$?
check "exit code passes through" 3 "$status"
check ".git config read-only" blocked "$(value git_config)"
check ".git hooks read-only" blocked "$(value git_hooks)"
check ".git can't be moved" blocked "$(value git_mv)"
check ".git/config unchanged on host" "$git_config_sum" "$(shasum "$WS/.git/config")"
check ".idea readable" idea "$(value idea_read)"
check ".idea read-only" blocked "$(value idea_write)"
check ".signing hidden" 0 "$(value signing_files)"
check "user namespace (not identity map)" no "$([[ "$(value uid_map)" == "0 0 4294967295" ]] && echo yes || echo no)"
check "no capabilities" 0000000000000000 "$(value cap_eff)"
check "/tmp on disk, not tmpfs" no "$([[ "$(value tmp_fs)" == tmpfs ]] && echo yes || echo no)"
check "allowed host reachable" yes "$([[ "$(value net_allowed)" != 000 ]] && echo yes || echo no)"
check "other host blocked" 000 "$(value net_denied)"
check "private address blocked" 000 "$(value net_private)"
check "plain HTTP refused" 403 "$(value net_http)"

echo "memory repository"
fresh memory
g -C "$MEM" init -q
echo '*.tmp' >"$MEM/.gitignore"
echo note >"$MEM/keep.md"
g -C "$MEM" add -A && g -C "$MEM" commit -qm init
HOME="$HOME_DIR" /bin/bash "$agent" trust-memory >/dev/null
run_agent bash -c '
    cd /memory
    if echo x >> .gitignore 2>/dev/null; then echo gitignore=writable; else echo gitignore=blocked; fi
    echo "x filter=evil" > .gitattributes
    mkdir -p .stuck/inner && echo x > .stuck/inner/f && chmod 555 .stuck/inner .stuck
    echo new > new.md; rm keep.md' >"$S/out" 2>&1 || true
check "memory .gitignore read-only" blocked "$(value gitignore)"
check "agent-made .gitattributes quarantined" no "$([[ -e "$MEM/.gitattributes" ]] && echo yes || echo no)"
check "agent's read-only folder still quarantined" no "$([[ -e "$MEM/.stuck" ]] && echo yes || echo no)"
check "quarantine reports the host path" yes \
    "$(grep -q "to $HOME_DIR/.agent-run/quarantine/" "$S/out" && echo yes || echo no)"
check "host git status shows the agent's changes" " D keep.md|?? new.md" \
    "$(git -C "$MEM" status --porcelain | sort | paste -sd'|' -)"

fresh fake-memory-repo
mkdir -p "$MEM/.git"
check "unrecorded memory .git refused" 1 "$(run_agent true >/dev/null 2>&1; echo $?)"

echo "concurrent agents"
# Agent A keeps writing to memory while agent B runs. B's two quarantines must
# pause A (and only while they run), resume it, and release the lock.
fresh concurrent
HOME="$HOME_DIR" WORKSPACE="$WS" /bin/bash "$agent" claude bash -c \
    'end=$((SECONDS + 40)); while [ $SECONDS -lt $end ]; do date +%s%N > /memory/a-tick.md; sleep 0.2; done' \
    </dev/null >/dev/null 2>&1 &
a_pid=$!
a_name=""
for _ in $(seq 60); do
    a_name="$(podman ps --format '{{.Names}}' | grep -E "^agent-run-claude-$a_pid-" || true)"
    [[ -n "$a_name" ]] && break
    perl -e 'select(undef, undef, undef, 0.5)'
done
since="$(date +%s)"
b_status=0
run_agent true >"$S/out" 2>&1 || b_status=$?
check "second agent runs while the first is running" 0 "$b_status"
# Podman's event log lags a little behind; give it a moment and look ahead.
perl -e 'select(undef, undef, undef, 2)'
until_ts=$(( $(date +%s) + 1 ))
pauses="$(podman events --since "$since" --until "$until_ts" --filter "container=$a_name" \
    --filter event=pause --format '{{.Status}}' </dev/null 2>/dev/null | wc -l | tr -d ' ')"
unpauses="$(podman events --since "$since" --until "$until_ts" --filter "container=$a_name" \
    --filter event=unpause --format '{{.Status}}' </dev/null 2>/dev/null | wc -l | tr -d ' ')"
check "first agent paused for both quarantines" 2 "$pauses"
check "and resumed each time" 2 "$unpauses"
check "first agent running again" running "$(podman inspect "$a_name" --format '{{.State.Status}}' 2>/dev/null)"
tick1="$(cat "$MEM/a-tick.md")"; perl -e 'select(undef, undef, undef, 1)'
check "first agent still writing" yes "$([[ "$(cat "$MEM/a-tick.md")" != "$tick1" ]] && echo yes || echo no)"
check "memory lock released" no "$([[ -e "$HOME_DIR/.agent-run/memory.lock" ]] && echo yes || echo no)"
kill -TERM "$a_pid"; wait "$a_pid" 2>/dev/null || true

echo "engram"
# A context repository next to the workspace, linked the way `engram setup`
# links a project: the workspace is project "ws" of ../engram-work.
fresh engram
E="$S/engram-work"
mkdir -p "$E/projects/ws/.memory/plans" "$E/projects/ws/.agents/skills" "$E/projects/ws/.claude/skills" \
    "$E/engram-core/.agents/skills/shared" "$E/engram-core/scripts"
echo '# context' >"$E/AGENTS.md"
printf '#!/bin/sh\n' >"$E/engram"
echo 'echo core script' >"$E/engram-core/scripts/engram-mcp.sh"
echo '# project ws' >"$E/projects/ws/AGENTS.md"
echo 'shared skill' >"$E/engram-core/.agents/skills/shared/SKILL.md"
ln -s ../../../../engram-core/.agents/skills/shared "$E/projects/ws/.agents/skills/shared"
ln -s ../../.agents/skills/shared "$E/projects/ws/.claude/skills/shared"
: >"$E/projects/ws/.memory/plans/.gitkeep"
git init -q "$E"
for overlay in AGENTS.md .memory .claude .agents; do
    ln -s "../engram-work/projects/ws/$overlay" "$WS/$overlay"
done
run_agent bash -c '
    cd /workspace
    echo "agents=$(head -1 AGENTS.md)"
    echo "skill=$(cat .claude/skills/shared/SKILL.md 2>&1)"
    if echo x >> AGENTS.md 2>/dev/null; then echo package=writable; else echo package=blocked; fi
    if echo "project note" > .memory/note.md 2>/dev/null; then echo memory=writable; else echo memory=blocked; fi
    echo planted > .memory/evil.sh
    echo "repo_git=$([ -e /engram-work/.git ] && echo visible || echo absent)"
    echo "core_scripts=$([ -e /engram-work/engram-core/scripts ] && echo visible || echo absent)"
    echo "entry=$([ -e /engram-work/engram ] && echo visible || echo absent)"' >"$S/out" 2>&1 || true
check "engram AGENTS.md reached through the link" "# project ws" "$(value agents)"
check "engram-core skill reached through the links" "shared skill" "$(value skill)"
check "engram package read-only" blocked "$(value package)"
check "engram memory writable" writable "$(value memory)"
check "note saved to the host's engram memory" "project note" "$(cat "$E/projects/ws/.memory/note.md" 2>/dev/null)"
check "agent-made script quarantined from engram memory" no "$([[ -e "$E/projects/ws/.memory/evil.sh" ]] && echo yes || echo no)"
check "empty .gitkeep kept" yes "$([[ -e "$E/projects/ws/.memory/plans/.gitkeep" ]] && echo yes || echo no)"
check "engram repository's .git not mounted" absent "$(value repo_git)"
check "engram-core scripts not mounted" absent "$(value core_scripts)"
check "engram entry point not mounted" absent "$(value entry)"
check "report shows how to save engram memory" yes \
    "$(grep -q "engram save ws" "$S/out" && echo yes || echo no)"
AGENT_ENGRAM=0 run_agent bash -c 'echo "optout=$([ -e /engram-work ] && echo mounted || echo absent)"' \
    >"$S/out" 2>&1 || true
check "AGENT_ENGRAM=0 leaves engram out" absent "$(value optout)"

echo "refusals"
fresh refuse
git -C "$WS" config core.hooksPath .husky
check "hooks path into workspace refused" 1 "$(run_agent true >/dev/null 2>&1; echo $?)"
if podman system connection list --format '{{.Name}}' | grep -qx podman-machine-default-root; then
    fresh rootful
    root_uri="$(podman system connection list --format '{{.Name}} {{.URI}}' |
        awk '$1 == "podman-machine-default-root" {print $2}')"
    check "rootful connection refused" 1 \
        "$(CONTAINER_HOST="$root_uri" run_agent true >/dev/null 2>&1; echo $?)"
fi

echo "signals and cleanup"
fresh signal
# Started directly, not through run_agent: a backgrounded function is a
# subshell, and $! would be its PID instead of the wrapper's.
HOME="$HOME_DIR" WORKSPACE="$WS" /bin/bash "$agent" claude sleep 120 </dev/null >/dev/null 2>&1 &
pid=$!
name=""
for _ in $(seq 60); do
    name="$(podman ps --format '{{.Names}}' | grep -E "^agent-run-claude-$pid-" || true)"
    [[ -n "$name" ]] && break
    perl -e 'select(undef, undef, undef, 0.5)'
done
check "container started" yes "$([[ -n "$name" ]] && echo yes || echo no)"
kill -TERM "$pid"
status=0
wait "$pid" || status=$?
check "TERM exits 143" 143 "$status"
check "no container left after TERM" "" "$(podman ps -a --format '{{.Names}}' | grep -E "^agent-run-claude-$pid-" || true)"

fresh early-signal
start=$(date +%s)
HOME="$HOME_DIR" WORKSPACE="$WS" /bin/bash "$agent" claude sleep 60 </dev/null >/dev/null 2>&1 &
pid=$!
perl -e 'select(undef, undef, undef, 1.0)'
kill -TERM "$pid"
status=0
wait "$pid" || status=$?
check "TERM before the container exists exits 143" 143 "$status"
check "and doesn't run the agent" yes "$([[ $(( $(date +%s) - start )) -lt 30 ]] && echo yes || echo no)"
check "no container left after early TERM" "" "$(podman ps -a --format '{{.Names}}' | grep -E "^agent-run-claude-$pid-" || true)"
check "no volumes left" "$volumes_before" "$(podman volume ls -q | wc -l | tr -d ' ')"

echo "proxy"
# 1.1.1.1's PTR record is one.one.one.one. With that name allowed, a CONNECT
# to the bare IP must still be refused: the allowlist must not match by
# reverse DNS (dstdomain -n).
echo one.one.one.one >"$scratch/ptr-allow.txt"
podman run -d --name agent-run-it-proxy -v "$here/../proxy/squid.conf:/etc/squid/squid.conf:ro,z" \
    -v "$scratch/ptr-allow.txt:/etc/squid/allowed-domains.txt:ro,z" localhost/agent-run-proxy >/dev/null
for _ in $(seq 20); do
    podman logs agent-run-it-proxy 2>&1 | grep -q 'Accepting HTTP' && break
    perl -e 'select(undef, undef, undef, 0.5)'
done
code() {
    podman run --rm --network container:agent-run-it-proxy localhost/agent-run-claude \
        curl -s -o /dev/null -m8 -w '%{http_code}' -x http://127.0.0.1:3128 "$1" </dev/null
}
check "allowed name passes" yes "$([[ "$(code https://one.one.one.one/)" != 000 ]] && echo yes || echo no)"
check "bare IP with an allowed PTR refused" 000 "$(code https://1.1.1.1/)"
podman rm -f agent-run-it-proxy >/dev/null

echo "$passed passed, $failed failed"
[[ $failed -eq 0 ]]
