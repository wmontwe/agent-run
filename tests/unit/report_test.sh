# lib/report.sh

setup_report() {
    new_repo ws
    mkdir -p tmp ws/src
    echo a >ws/src/a.txt
    echo 'export X=1' >ws/.envrc
}

report() {
    report_print "$T/ws" "$T/tmp" "${1:-true}" false "$T/mem" 2>&1
}

test_no_changes() {
    setup_report
    report_snapshot "$T/ws" "$T/tmp"
    out="$(report)"
    assert_contains "Other changed files: 0" "$out"
    assert_not_contains "Created or changed" "$out"
    assert_not_contains "Deleted:" "$out"
}

test_sensitive_changes_and_deletions() {
    setup_report
    report_snapshot "$T/ws" "$T/tmp"
    rm ws/.envrc
    mkdir -p ws/.claude && echo '{}' >ws/.claude/settings.local.json
    echo x >ws/git-town.toml
    echo b >>ws/src/a.txt
    out="$(report)"
    assert_contains "Deleted:"$'\n'"  .envrc" "$out"
    assert_contains "  .claude/settings.local.json" "$out"
    assert_contains "  git-town.toml" "$out"
    assert_not_contains "  src/a.txt" "$out"
}

test_ignored_files_reported() {
    setup_report
    report_snapshot "$T/ws" "$T/tmp"
    echo .mcp.json >>ws/.gitignore
    echo '{}' >ws/.mcp.json
    out="$(report)"
    assert_contains "  .gitignore" "$out"
    assert_contains "  .mcp.json" "$out"
}

test_nested_git_always_reported() {
    setup_report
    new_repo ws/old
    report_snapshot "$T/ws" "$T/tmp"
    mkdir -p ws/sub/.git
    out="$(report)"
    assert_contains "  old/.git" "$out"
    assert_contains "  sub/.git" "$out"
    assert_not_contains "  .git"$'\n' "$out"
}

test_names_escaped() {
    setup_report
    report_snapshot "$T/ws" "$T/tmp"
    mkdir -p ws/.vscode && touch "ws/.vscode/evil"$'\n'"name"
    out="$(report)"
    assert_contains "\$'.vscode/evil\\nname'" "$out"
}

test_incomplete_scan_reported() {
    setup_report
    report_snapshot "$T/ws" "$T/tmp"
    mkdir ws/locked && chmod 000 ws/locked
    out="$(report)"
    chmod 755 ws/locked
    assert_contains "Scan incomplete" "$out"
}

test_memory_review_commands() {
    setup_report
    report_snapshot "$T/ws" "$T/tmp"
    out="$(report_print "$T/ws" "$T/tmp" true true "$T/mem" 2>&1)"
    assert_contains "Review memory before committing it:" "$out"
}

# Review finding: only files count, not the directories whose ctime changed.
test_other_count_is_files_only() {
    setup_report
    report_snapshot "$T/ws" "$T/tmp"
    mkdir -p ws/a/b && echo x >ws/a/b/c.txt
    out="$(report)"
    assert_contains "Other changed files: 1" "$out"
}

test_new_sensitive_names_reported() {
    setup_report
    report_snapshot "$T/ws" "$T/tmp"
    mkdir -p ws/.husky ws/app/gradle/wrapper
    echo x >ws/.husky/pre-commit
    echo 'script-shell=./x.sh' >ws/.npmrc
    echo url >ws/app/gradle/wrapper/gradle-wrapper.properties
    echo X=1 >ws/.env.local
    echo all: >ws/Makefile
    echo "# lock" >ws/mise.lock
    out="$(report)"
    for path in .husky/pre-commit .npmrc app/gradle/wrapper/gradle-wrapper.properties .env.local Makefile mise.lock; do
        assert_contains "  $path" "$out"
    done
    assert_contains "Other changed files: 0" "$out"
}

test_nested_git_internals_not_listed() {
    setup_report
    report_snapshot "$T/ws" "$T/tmp"
    mkdir -p ws/sub/.git/hooks && echo x >ws/sub/.git/hooks/pre-commit
    out="$(report)"
    assert_contains "  sub/.git" "$out"
    assert_not_contains "hooks/pre-commit" "$out"
}

test_find_tests_match_shell_matcher() {
    local path
    for path in a/.envrc a/.env.prod a/.claude a/.claude/x.json a/b/.husky/pre-push a/Makefile; do
        assert_ok is_sensitive "$T/$path"
    done
    for path in a/env a/src/main.kt a/claude/x a/Makefile.bak; do
        assert_dies "" is_sensitive "$T/$path"
    done
    mkdir -p f/.husky f/src && touch f/.husky/pre-commit f/.env.prod f/src/main.kt
    local listed=0
    while IFS= read -r -d '' path; do
        assert_ok is_sensitive "$path"
        listed=$((listed + 1))
    done < <(find -P "$T/f" "${SENSITIVE_TESTS[@]}" -print0)
    assert_eq 3 "$listed"
}

# Folders the agent's own mounts cover are the host's: neither scan reports
# what changes or disappears there.
test_covered_folders_skipped() {
    setup_report
    mkdir -p ws/app/build/tmp/clone ws/app/src
    echo x >ws/app/build/tmp/clone/AGENTS.md
    report_snapshot "$T/ws" "$T/tmp" "$T/ws/app/build"
    rm ws/app/build/tmp/clone/AGENTS.md
    mkdir -p ws/app/build/tmp/other/.git
    echo y >ws/app/build/.envrc
    echo z >ws/app/src/AGENTS.md
    out="$(report)"
    assert_not_contains "app/build" "$out"
    assert_not_contains "Deleted:" "$out"
    assert_contains "  app/src/AGENTS.md" "$out"
}
