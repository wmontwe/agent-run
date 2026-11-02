# A lock around everything that must not overlap with another agent writing
# to shared memory: the memory quarantine (with the other agents paused) and
# the start of an agent container. The lock is a directory, since mkdir is
# atomic; it lives outside every mount.

# memory_lock DIR TIMEOUT: takes the lock DIR, waiting up to TIMEOUT seconds.
# A lock whose holder process is gone is stale and taken over. Stops waiting
# (exits) once bin/agent's signal traps have set $signal.
memory_lock() {
    local dir="$1" timeout="$2" waited=0 holder
    while ! mkdir -- "$dir" 2>/dev/null; do
        [[ ${signal:-0} -eq 0 ]] || exit "$signal"
        holder="$(cat "$dir/pid" 2>/dev/null || true)"
        if [[ -n "$holder" ]] && ! kill -0 "$holder" 2>/dev/null; then
            rm -rf -- "$dir"
            continue
        fi
        [[ $waited -ne 0 ]] ||
            echo "agent: waiting for another agent-run process (${holder:-?}) to finish with memory" >&2
        [[ $waited -lt $((timeout * 4)) ]] ||
            die "memory is locked by process ${holder:-?} ($dir); remove that folder if the process is gone"
        sleep 0.25
        waited=$((waited + 1))
    done
    echo "$$" >"$dir/pid"
}

# memory_unlock DIR: releases the lock, if this process holds it.
memory_unlock() {
    [[ "$(cat "$1/pid" 2>/dev/null || true)" == "$$" ]] && rm -rf -- "$1"
    return 0
}
