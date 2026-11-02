# The advisory change report. It is built with find, not Git, so .gitignore
# can't hide anything, and it never modifies workspace files.

# Paths the host may execute or load: file names (glob patterns) and folders,
# at any depth. Both the find tests and the shell matcher are built from these
# two lists, so they can't drift apart.
SENSITIVE_NAMES=(AGENTS.md CLAUDE.md GEMINI.md .envrc .env '.env.*' .mcp.json .gitmodules .gitattributes .gitignore
    git-town.toml .git-town.toml .pre-commit-config.yaml lefthook.yml .lefthook.yml
    lefthook-local.yml .npmrc .yarnrc.yml mise.toml .mise.toml .tool-versions .nvmrc
    Makefile package.json gradlew gradle-wrapper.properties gradle-wrapper.jar mvnw)
SENSITIVE_DIRS=(.claude .codex .gemini .agents .junie .air .vscode .idea .signing .husky)

SENSITIVE_TESTS=( \( )
for _name in "${SENSITIVE_NAMES[@]}"; do
    SENSITIVE_TESTS+=(-name "$_name" -o)
done
for _name in "${SENSITIVE_DIRS[@]}"; do
    SENSITIVE_TESTS+=(-name "$_name" -o -path "*/$_name/*" -o)
done
unset '_name'
SENSITIVE_TESTS[${#SENSITIVE_TESTS[@]}-1]=')'

# is_sensitive PATH: the shell version of SENSITIVE_TESTS.
is_sensitive() {
    local base="${1##*/}" pattern
    for pattern in "${SENSITIVE_NAMES[@]}"; do
        # shellcheck disable=SC2254
        case "$base" in $pattern) return 0 ;; esac
    done
    for pattern in "${SENSITIVE_DIRS[@]}"; do
        case "$1" in */"$pattern" | */"$pattern"/*) return 0 ;; esac
    done
    return 1
}

# list_paths WS TESTS...: prints NUL-delimited paths below WS matching the find
# TESTS, skipping the protected top-level .git. Fails if find fails.
list_paths() {
    local ws="$1"
    shift
    find -P "$ws" -path "$ws/.git" -prune -o "$@" -print0
}

# report_snapshot WS TMP: records the sensitive paths that exist now, so
# deletions can be reported, then the start time. Fails if find fails.
report_snapshot() {
    local rc=0
    list_paths "$1" "${SENSITIVE_TESTS[@]}" >"$2/before" || rc=$?
    : >"$2/start"
    return $rc
}

# report_print WS TMP COMPLETE MEM_REPO MEM: prints the report to stderr.
# COMPLETE is false if an earlier scan already failed. One walk lists every
# nested .git (whatever its age, without descending into it) and everything
# changed since the snapshot.
report_print() {
    local ws="$1" tmp="$2" complete="$3" mem_repo="$4" mem="$5" path others=0
    : >"$tmp/nested" && : >"$tmp/changed" && : >"$tmp/deleted"
    list_paths "$ws" \( -name .git -print0 -prune \) -o -cnewer "$tmp/start" \
        >"$tmp/scan" || complete=false
    while IFS= read -r -d '' path; do
        if [[ "${path##*/}" == .git ]]; then
            printf '%s\0' "$path" >>"$tmp/nested"
        elif is_sensitive "$path"; then
            printf '%s\0' "$path" >>"$tmp/changed"
        elif [[ -L "$path" || ! -d "$path" ]]; then
            others=$((others + 1))
        fi
    done <"$tmp/scan"
    while IFS= read -r -d '' path; do
        [[ -e "$path" || -L "$path" ]] || printf '%s\0' "$path" >>"$tmp/deleted"
    done <"$tmp/before"
    _report_show() {
        local path
        while IFS= read -r -d '' path; do
            printf '  %q\n' "${path#"$ws"/}"
        done
    }
    {
        echo
        echo "agent-run change report for $ws (advisory, may be incomplete)"
        if [[ -s "$tmp/nested" ]]; then
            echo "Nested .git entries (host Git would use their config and hooks):"
            _report_show <"$tmp/nested"
        fi
        if [[ -s "$tmp/changed" ]]; then
            echo "Created or changed, may be loaded or executed by host tools:"
            _report_show <"$tmp/changed"
        fi
        if [[ -s "$tmp/deleted" ]]; then
            echo "Deleted:"
            _report_show <"$tmp/deleted"
        fi
        echo "Other changed files: $others"
        if [[ $complete != true ]]; then
            echo "Scan incomplete: find reported errors (see above); some changes may be missing."
        fi
        echo "Review before running anything:"
        echo "  git status --untracked-files=all --ignored"
        echo "  git --no-pager diff --no-ext-diff --no-textconv"
        if [[ $mem_repo == true ]]; then
            echo "Review memory before committing it:"
            printf '  git -C %q status --untracked-files=all --ignored\n' "$mem"
            printf '  git -C %q --no-pager diff --no-ext-diff --no-textconv\n' "$mem"
        fi
    } >&2
}

# report_engram ROOT PROJECT: prints, to stderr, how to review and save the
# project's engram memory on the host.
report_engram() {
    {
        echo "Review engram memory before saving it:"
        printf '  git -C %q status --untracked-files=all --ignored -- %q\n' "$1" "projects/$2/.memory"
        printf '  git -C %q --no-pager diff --no-ext-diff --no-textconv -- %q\n' "$1" "projects/$2/.memory"
        printf '  %q save %q\n' "$1/engram" "$2"
    } >&2
}
