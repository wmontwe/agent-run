# Git settings that make host Git load or run files from the workspace or
# memory would let the agent change what host Git runs, so refuse them in
# either repository, in any scope. Only a protected (read-only) .git inside
# them is fine. Enforced: core.hooksPath and include.path / includeIf.*.path.
# Best effort: settings whose value is a command (fsmonitor, editors, pagers,
# diff/filter/merge drivers, credential helpers, ! aliases, ...) are refused
# if a path-like word in the command points into either folder; a shell
# command can still reach one in ways this can't see. Includes are followed
# only into files outside both (read with --no-includes, so no agent-writable
# file is ever read), and Git runs without a pager, so nothing is executed.

GIT_PATH_KEYS='core\.hookspath|include\.path|includeif\..*\.path'
GIT_COMMAND_KEYS='core\.(fsmonitor|sshcommand|editor|pager|askpass|gitproxy)|sequence\.editor'
GIT_COMMAND_KEYS="$GIT_COMMAND_KEYS"'|gpg(\..*)?\.program|credential(\..*)?\.helper|alias\..*|pager\..*'
GIT_COMMAND_KEYS="$GIT_COMMAND_KEYS"'|diff\..*\.(command|textconv)|(diff|merge)tool\..*\.(cmd|path)'
GIT_COMMAND_KEYS="$GIT_COMMAND_KEYS"'|merge\..*\.driver|filter\..*\.(clean|smudge|process)'
GIT_COMMAND_KEYS="$GIT_COMMAND_KEYS"'|uploadpack\.packobjectshook|sendemail\..*'
GIT_COMMAND_KEYS="$GIT_COMMAND_KEYS"'|diff\.external|interactive\.difffilter|gpg\.ssh\.defaultkeycommand'
GIT_COMMAND_KEYS="$GIT_COMMAND_KEYS"'|core\.alternaterefscommand|trailer\..*\.(command|cmd)'
GIT_COMMAND_KEYS="$GIT_COMMAND_KEYS"'|remote\..*\.(uploadpack|receivepack)|tar\..*\.command'
GIT_COMMAND_KEYS="$GIT_COMMAND_KEYS"'|(browser|man)\..*\.(cmd|path)|submodule\..*\.update'
GIT_GUARD_KEYS="^($GIT_PATH_KEYS|$GIT_COMMAND_KEYS)\$"

# git_guard_init WS MEM MEM_REPO TMP [WRITABLE...]: sets the folders the guard
# protects and where it keeps its temporary output. WRITABLE are further
# agent-writable folders without a .git of their own (engram's .memory). Call
# before the other functions.
git_guard_init() {
    GIT_GUARD_WS="$1" GIT_GUARD_MEM="$2" GIT_GUARD_MEM_REPO="$3" GIT_GUARD_TMP="$4"
    shift 4
    GIT_GUARD_EXTRA=()
    [[ $# -eq 0 ]] || GIT_GUARD_EXTRA=("$@")
}

# True if PATH is inside the workspace, memory or another agent-writable
# folder, outside the protected .git of the workspace or a memory repository.
agent_writable() {
    local root
    for root in ${GIT_GUARD_EXTRA[@]+"${GIT_GUARD_EXTRA[@]}"}; do
        [[ "$1" == "$root" || "$1" == "$root"/* ]] && return 0
    done
    for root in "$GIT_GUARD_WS" "$GIT_GUARD_MEM"; do
        if [[ "$1" == "$root/.git" || "$1" == "$root/.git/"* ]]; then
            [[ -d "$root/.git" && ! -L "$root/.git" ]] || return 0
            [[ "$root" == "$GIT_GUARD_WS" || $GIT_GUARD_MEM_REPO == true ]] || return 0
        elif [[ "$1" == "$root" || "$1" == "$root"/* ]]; then
            return 0
        fi
    done
    return 1
}

# check_git_command KEY VALUE ORIGIN REPO: refuses a command-valued setting if
# a path-like word in it (containing a slash, or starting with ~) resolves
# into the workspace or memory.
check_git_command() {
    local key="$1" value="${2#!}" origin="$3" repo="$4" word target
    local IFS=$' \t\n;|&()<>=`"\''
    set -f
    for word in $value; do
        case "$word" in */*|"~"*) ;; *) continue ;; esac
        target="$(resolve_path "$word" "$repo")" || {
            set +f
            die "Git setting $key = $2 (from $origin, in $repo) names $word, which can't be resolved (symlink loop?)"
        }
        if agent_writable "$target"; then
            set +f
            die "Git setting $key = $2 (from $origin, in $repo) runs $word from the workspace or memory, so an agent could change what host Git runs; move it elsewhere or remove it"
        fi
    done
    set +f
}

# check_git_config DEPTH REPO [FILE]: checks REPO's effective configuration
# (depth 0, no FILE) or one included FILE outside the agent-writable folders
# (depth > 0). Fails closed if Git can't read its configuration.
check_git_config() {
    local depth="$1" repo="$2" file="${3:-}" out="$GIT_GUARD_TMP/git-config.$1" rc=0
    local origin entry key value base target
    [[ $depth -le 10 ]] || die "Git includes nested more than 10 levels deep"
    if [[ -z "$file" ]]; then
        git --no-pager -C "$repo" config --no-includes --show-origin --null \
            --get-regexp "$GIT_GUARD_KEYS" >"$out" 2>"$GIT_GUARD_TMP/git-config.err" || rc=$?
    else
        git --no-pager config --file "$file" --no-includes --show-origin --null \
            --get-regexp "$GIT_GUARD_KEYS" >"$out" 2>"$GIT_GUARD_TMP/git-config.err" || rc=$?
    fi
    [[ $rc -eq 0 || $rc -eq 1 ]] ||
        die "could not read Git configuration: $(cat "$GIT_GUARD_TMP/git-config.err")"
    while IFS= read -r -d '' origin && IFS= read -r -d '' entry; do
        key="${entry%%$'\n'*}" value="${entry#*$'\n'}"
        # Git prints the repository's own config as a relative path.
        case "$origin" in
            file:/*) origin="${origin#file:}"; base="${origin%/*}" ;;
            file:*) origin="${origin#file:}"; base="$repo/${origin%/*}" ;;
            *) base="$repo" ;;
        esac
        case "$key" in
            core.hookspath|include.path|includeif.*.path) ;;
            *) check_git_command "$key" "$value" "$origin" "$repo"; continue ;;
        esac
        [[ "$key" == core.hookspath ]] && base="$repo"
        target="$(resolve_path "$value" "$base")" ||
            die "Git setting $key = $value (from $origin, in $repo) can't be resolved (symlink loop?)"
        if agent_writable "$target"; then
            die "Git setting $key = $value (from $origin, in $repo) points into the workspace or memory, so an agent could change what host Git runs; move it elsewhere or remove it"
        fi
        if [[ "$key" != core.hookspath && -f "$target" ]]; then
            check_git_config $((depth + 1)) "$repo" "$target"
        fi
    done <"$out"
}
