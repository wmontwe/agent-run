# Podman and Compose calls. Everything else in bin/agent runs without Podman.

# /tmp and /var/tmp are anonymous volumes on the Podman VM's disk: one per
# container, removed with it. A compose `- /tmp` entry would instead become a
# named volume shared across runs. Podman's automatic read-only tmpfs mounts
# are disabled, since with keep-id they would cover the volumes (and use RAM);
# /dev/shm gets a 256 MB tmpfs back, enough for Chromium.
AGENT_RUN_ARGS="--read-only-tmpfs=false --mount type=volume,dst=/tmp \
--mount type=volume,dst=/var/tmp --mount type=tmpfs,dst=/dev/shm,tmpfs-size=256m,tmpfs-mode=1777"

# Rootful Podman ignores keep-id: the container user is then a real VM user and
# an escape lands as VM root, which can reach the host share. Require rootless.
require_rootless() {
    local rootless
    rootless="$(podman info --format '{{.Host.Security.Rootless}}' 2>/dev/null)" ||
        die "cannot reach Podman; is the machine running? (podman machine start)"
    [[ "$rootless" == true ]] ||
        die "the Podman connection is rootful; use a rootless one (podman machine set --rootful=false, or podman system connection default)"
}

# compose ROOT WS ARGS...: runs podman compose on ROOT's compose.yaml.
# --env-file /dev/null stops podman-compose from loading .env from the compose
# directory or the current directory; the caller runs it from ROOT.
compose() {
    local root="$1" ws="$2"
    shift 2
    WORKSPACE="$ws" PODMAN_COMPOSE_WARNING_LOGS=false \
        podman compose --env-file /dev/null -f "$root/compose.yaml" "$@"
}

# new_container_name AGENT: sets CONTAINER_NAME. A random suffix keeps the
# name unique even if a crashed run's container with a reused PID is still
# around; cleanup must only ever hit our own.
new_container_name() {
    CONTAINER_NAME="agent-run-$1-$$-$(od -An -N4 -tx4 /dev/urandom | tr -d ' \n')"
    if podman container exists "$CONTAINER_NAME" 2>/dev/null; then
        die "a container named $CONTAINER_NAME already exists; try again"
    fi
}

# stop_container NAME: stops and removes NAME with its anonymous volumes.
stop_container() {
    podman stop --time 10 "$1" >/dev/null 2>&1 || true
    podman rm --force --volumes "$1" >/dev/null 2>&1 || true
}

# quarantine_in_container IMAGE ROOT PHASE MEM QROOT MEM_REPO [PROTECTED...]:
# runs quarantine_memory in a throwaway container that only sees memory, the
# quarantine folder and lib/ (read-only), with no network and no capabilities.
# A concurrently running agent can swap a memory folder for a symlink between
# listing and moving; inside this container, that link can only point at the
# container's own paths, never at host files. Fails if the container can't run.
quarantine_in_container() {
    local image="$1" root="$2" phase="$3" mem="$4" qroot="$5" mem_repo="$6" path
    shift 6
    local protected=()
    for path in "$@"; do
        protected+=("/memory/${path#"$mem"/}")
    done
    mkdir -p -- "$qroot"
    podman run --rm --network none --read-only --cap-drop ALL \
        --security-opt no-new-privileges --userns keep-id:uid=1000,gid=1000 \
        -v "$mem:/memory:z" -v "$qroot:/quarantine:z" -v "$root/lib:/opt/agent-run/lib:ro,z" \
        -e QUARANTINE_ID="$$" -e QUARANTINE_DISPLAY="$qroot" \
        --entrypoint bash "$image" -c \
        '. /opt/agent-run/lib/common.sh && . /opt/agent-run/lib/memory.sh && quarantine_memory "$@"' \
        quarantine "$phase" /memory /quarantine /tmp "$mem_repo" \
        ${protected[@]+"${protected[@]}"} </dev/null
}

# pause_memory_users MEM_PATH...: pauses every running container that has one
# of the MEM_PATHs mounted (agents started by bin/agent or plain `podman
# compose run`), so none can change memory while it is quarantined. Sets
# PAUSED to the containers paused; resume_paused undoes it. Fails closed:
# if one can't be paused, the others are resumed and the run stops.
pause_memory_users() {
    local ids mounts name sources path
    PAUSED=()
    ids="$(podman ps -q --filter status=running)" || die "could not list running containers"
    [[ -n "$ids" ]] || return 0
    # shellcheck disable=SC2086 # one argument per container ID
    mounts="$(podman inspect --format '{{.Name}}|{{range .Mounts}}{{.Source}};{{end}}' $ids)" ||
        die "could not inspect running containers"
    while IFS='|' read -r name sources; do
        for path in "$@"; do
            case ";$sources" in
                *";$path;"*)
                    if podman pause "$name" >/dev/null 2>&1; then
                        PAUSED+=("$name")
                    else
                        resume_paused
                        die "could not pause $name, which has memory mounted"
                    fi
                    break ;;
            esac
        done
    done <<<"$mounts"
}

# resume_paused: unpauses what pause_memory_users paused.
resume_paused() {
    local name
    for name in ${PAUSED[@]+"${PAUSED[@]}"}; do
        podman unpause "$name" >/dev/null 2>&1 ||
            echo "agent: could not unpause $name; run: podman unpause $name" >&2
    done
    PAUSED=()
}

# prepare_gradle_state IMAGE VOLUME PATHS...: creates VOLUME and the PATHS in
# it, since Podman only mounts existing subpaths. In a container without
# network, as the agent's user, so the agent owns them.
prepare_gradle_state() {
    local image="$1" volume="$2"
    shift 2
    podman volume exists "$volume" || podman volume create "$volume" >/dev/null ||
        die "could not create volume $volume"
    podman run --rm --network none --read-only --cap-drop ALL \
        --security-opt no-new-privileges --userns keep-id:uid=1000,gid=1000 \
        -v "$volume:/state" --entrypoint sh "$image" \
        -c 'cd /state && mkdir -p -- "$@"' prepare "$@" </dev/null ||
        die "could not prepare volume $volume"
}
