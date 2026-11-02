# Shared helpers for bin/agent. Libraries are sourced only from the resolved
# launcher root, which no agent can reach; they set no shell options (the
# caller runs with `set -euo pipefail`) and must stay macOS bash 3.2 compatible.

die() {
    printf 'agent: %s\n' "$*" >&2
    exit 1
}

# Prints the physical path of directory PATH, or fails if it isn't one.
canonical() {
    (cd -P -- "$1" 2>/dev/null && pwd -P)
}

# True if either path is the other or contains it.
overlaps() {
    [[ "$1" == / || "$2" == / || "$1" == "$2" || "$1" == "$2"/* || "$2" == "$1"/* ]]
}

# check_path_chars LABEL PATH: refuses characters that would corrupt a -v
# mount argument, Compose interpolation or a find -path pattern.
check_path_chars() {
    case "$2" in
        *[:,\*\?\[\]\\]* | *$'\n'*)
            die "$1 path contains an unsupported character (: , * ? [ ] \\ or newline): $2" ;;
    esac
}

# resolve_path PATH BASE: prints PATH as the physical path it refers to: ~
# expanded, relative to BASE, and every symlink resolved, including ones whose
# targets don't exist yet. After each symlink the whole result is resolved
# again, so a link to a symlinked or not-yet-existing location still comes out
# canonical. Components that don't exist yet are appended as they are. Fails
# after 40 links (the kernel's limit), e.g. on a symlink loop.
resolve_path() {
    local path="$1" base="$2" rest prefix link dir hops=0
    case "$path" in
        "~") path="$HOME" ;;
        "~/"*) path="$HOME/${path#\~/}" ;;
        /*) ;;
        *) path="$base/$path" ;;
    esac
    while :; do
        # The longest prefix that exists (as a file, directory or link).
        prefix="$path" rest=""
        while [[ -n "$prefix" && ! -e "$prefix" && ! -L "$prefix" ]]; do
            rest="/${prefix##*/}$rest"
            prefix="${prefix%/*}"
        done
        [[ -n "$prefix" ]] || { printf '%s\n' "$rest"; return 0; }
        if [[ -L "$prefix" ]]; then
            hops=$((hops + 1))
            [[ $hops -le 40 ]] || return 1
            link="$(readlink -- "$prefix")"
            [[ "$link" == /* ]] || link="${prefix%/*}/$link"
            path="$link$rest"
            continue
        fi
        # The prefix exists and isn't a link; canonical resolves links above it.
        if [[ -d "$prefix" ]]; then
            dir="$(canonical "$prefix")" || return 1
            printf '%s\n' "$dir$rest"
        else
            dir="$(canonical "${prefix%/*}")" || return 1
            printf '%s\n' "$dir/${prefix##*/}$rest"
        fi
        return 0
    done
}
