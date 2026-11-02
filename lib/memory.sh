# Shared memory may only hold plain notes: directories and regular .md and
# .jsonl files, with safe names that don't start with "." or "-". No .json:
# too many tools load JSON configuration. Anything else (symlinks, FIFOs,
# dotfiles, scripts) is moved, never followed or deleted, into quarantine.
# bin/agent runs this before launch (another agent may have written since) and
# after the run. A memory repository's protected .git, .gitignore and
# .gitattributes are left alone; only a real repository's .git is skipped, an
# agent-made one is not.

# quarantine_memory PHASE MEM QROOT TMP MEM_REPO [PROTECTED...]: moves
# everything not allowed in MEM to QROOT/<time>-<id>-<PHASE>/, keeping its
# path, and lists it on stderr. Never fails: problems are reported instead.
# bin/agent runs it inside a throwaway container (quarantine_in_container), so
# a symlink swapped in by a concurrently running agent can't make it move host
# files; QUARANTINE_ID and QUARANTINE_DISPLAY then give the run's ID and the
# host path to show for QROOT.
quarantine_memory() {
    local phase="$1" mem="$2" qroot="$3" tmp="$4" mem_repo="$5"
    shift 5
    local protected=() prune=() path base reason dest qdir="" failed=false item skip LC_ALL=C
    [[ $# -eq 0 ]] || protected=("$@")
    [[ $mem_repo == true ]] && prune=(-path "$mem/.git" -prune -o)
    while IFS= read -r -d '' path; do
        # Parents are listed first; skip children of an already moved directory.
        [[ -e "$path" || -L "$path" ]] || continue
        skip=false
        for item in ${protected[@]+"${protected[@]}"}; do
            [[ "$path" == "$item" ]] && skip=true
        done
        [[ $skip == true ]] && continue
        base="${path##*/}" reason=""
        case "$base" in
            .*|-*) reason="name starts with . or -" ;;
            *[!A-Za-z0-9\ ._-]*) reason="unsupported character in name" ;;
        esac
        if [[ -z "$reason" ]]; then
            if [[ -L "$path" ]]; then
                reason="symlink"
            elif [[ -d "$path" ]]; then
                continue
            elif [[ ! -f "$path" ]]; then
                reason="not a regular file"
            else
                case "$base" in
                    *.md|*.jsonl) continue ;;
                    *) reason="not .md or .jsonl" ;;
                esac
            fi
        fi
        if [[ -z "$qdir" ]]; then
            qdir="$qroot/$(date -u +%Y%m%dT%H%M%SZ)-${QUARANTINE_ID:-$$}-$phase"
            mkdir -p -- "$qdir"
            printf 'Quarantined from memory (%s run) to %s:\n' "$phase" \
                "${QUARANTINE_DISPLAY:-$qroot}/${qdir#"$qroot"/}" >&2
        fi
        dest="$qdir/${path#"$mem"/}"
        # Moving into another mount copies and deletes, which an agent could
        # block by making a folder read-only; its files are ours, so undo that.
        if [[ -d "$path" && ! -L "$path" ]]; then
            chmod -R u+rwX -- "$path" 2>/dev/null || true
        fi
        if mkdir -p -- "${dest%/*}" && mv -- "$path" "$dest"; then
            printf '  %q (%s)\n' "${path#"$mem"/}" "$reason" >&2
        else
            printf '  %q (%s): could not be moved\n' "${path#"$mem"/}" "$reason" >&2
            failed=true
        fi
    done < <(find -P "$mem" -mindepth 1 ${prune[@]+"${prune[@]}"} -print0 ||
        echo >"$tmp/memory-failed")
    if [[ -e "$tmp/memory-failed" ]]; then
        rm -f -- "$tmp/memory-failed"
        failed=true
    fi
    if [[ $failed == true ]]; then
        echo "Memory check incomplete: see errors above." >&2
    fi
    return 0
}
