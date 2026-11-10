# Gradle's build state in the workspace, which the agent gets its own copy of:
# the host's Gradle and IDE load these folders, and two builds on one folder
# from different machines can't coordinate (their locks don't cross the VM).

# find_gradle_state WS TMP: sets GRADLE_STATE to the folders, relative to WS,
# that bin/agent covers with the agent's own: .gradle and .kotlin next to each
# settings script, build next to each build script. Missing ones are listed
# too, so the agent's Gradle never creates them in the workspace. Fails on a
# name that can't be mounted safely, or on a symlink or file in their place.
find_gradle_state() {
    local ws="$1" tmp="$2" file dir path
    GRADLE_STATE=()
    find -P "$ws" \( -name .git -o -name build -o -name .gradle -o -name .kotlin \
        -o -name node_modules \) -prune -o -type f \( -name settings.gradle \
        -o -name settings.gradle.kts -o -name build.gradle -o -name build.gradle.kts \) \
        -print0 >"$tmp/gradle-scripts" || die "could not look for Gradle scripts in $ws"
    : >"$tmp/gradle-state"
    while IFS= read -r -d '' file; do
        dir="${file%/*}"
        dir="${dir#"$ws"}"
        dir="${dir#/}"
        dir="${dir:+$dir/}"
        case "${file##*/}" in
            settings.*) printf '%s\0' "$dir.gradle" "$dir.kotlin" >>"$tmp/gradle-state" ;;
            *) printf '%s\0' "${dir}build" >>"$tmp/gradle-state" ;;
        esac
    done <"$tmp/gradle-scripts"
    while IFS= read -r -d '' path; do
        # The paths go into --mount arguments, split on commas.
        [[ "$path" =~ ^[A-Za-z0-9._/-]+$ ]] ||
            die "$(printf '%q' "$ws/$path"): unsupported characters for the agent's Gradle state"
        check_protectable_dir "$ws/$path"
        GRADLE_STATE+=("$path")
    done < <(sort -zu "$tmp/gradle-state")
}

# workspace_volume AGENT KIND WS: prints the name of AGENT's KIND volume for
# WS, kept between runs: "build" holds the Gradle state above, so incremental
# builds survive; "gradle" is the Gradle home (~/.gradle). Builds load code
# from it (init scripts, cached plugins and jars), so an agent working on one
# workspace mustn't plant it for another. Sharing would also block concurrent
# builds: Gradle releases its locks through a localhost ping, which doesn't
# reach another container.
workspace_volume() {
    printf 'agent-run-%s-%s-%s' "$1" "$2" "$(printf '%s' "$3" | shasum | cut -c1-12)"
}

# gradle_state_mounts VOLUME WS PATHS...: sets GRADLE_STATE_MOUNTS to the
# agent container's mounts of VOLUME's subpaths over WS/PATH, as `podman run`
# arguments in one string. podman-compose splits it with shlex, so each
# argument is single-quoted.
gradle_state_mounts() {
    local volume="$1" ws="$2" path arg quote="'\\''"
    shift 2
    GRADLE_STATE_MOUNTS=""
    for path in "$@"; do
        arg="type=volume,src=$volume,dst=$ws/$path,subpath=$path"
        GRADLE_STATE_MOUNTS="$GRADLE_STATE_MOUNTS --mount '${arg//\'/$quote}'"
    done
}
