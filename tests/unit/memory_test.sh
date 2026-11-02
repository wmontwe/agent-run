# lib/memory.sh

# quarantine MEM_REPO [PROTECTED...]: runs the "after" phase on $T/mem.
quarantine() {
    mkdir -p tmp
    quarantine_memory after "$T/mem" "$T/q" "$T/tmp" "$@"
}

test_allowed_notes_stay() {
    mkdir -p mem/projects/app
    echo n >mem/README.md
    echo '{}' >mem/projects/app/log.jsonl
    echo n >"mem/projects/app/with space.md"
    out="$(quarantine false 2>&1)"
    assert_eq "" "$out"
    assert_exists mem/projects/app/log.jsonl
    assert_exists "mem/projects/app/with space.md"
}

test_disallowed_files_moved() {
    mkdir -p mem/projects .mem-target
    echo x >mem/evil.sh
    echo '{}' >mem/projects/package.json
    echo '{}' >mem/state.json
    touch -- mem/-flag.md
    mkdir -p mem/.obsidian/plugins && echo '{}' >mem/.obsidian/plugins/p.json
    out="$(quarantine false 2>&1)"
    for path in evil.sh projects/package.json state.json -flag.md .obsidian; do
        assert_missing "mem/$path"
        assert_contains "$path" "$out"
    done
    q="$(echo "$T"/q/*-after)"
    assert_exists "$q/projects/package.json"
    assert_exists "$q/.obsidian/plugins/p.json"
}

test_symlink_moved_not_followed() {
    mkdir mem
    echo secret >target.txt
    ln -s "$T/target.txt" mem/link.md
    out="$(quarantine false 2>&1)"
    assert_contains "link.md (symlink)" "$out"
    assert_eq secret "$(cat target.txt)"
    [[ -L "$(echo "$T"/q/*-after)/link.md" ]] || fail "expected the link itself in quarantine"
}

test_special_files_and_hostile_names() {
    mkdir mem
    mkfifo mem/q.jsonl
    touch "mem/bad"$'\n'"name.md"
    out="$(quarantine false 2>&1)"
    assert_contains "q.jsonl (not a regular file)" "$out"
    assert_contains "\$'bad\\nname.md'" "$out"
}

test_agent_made_git_quarantined_without_repo() {
    mkdir -p mem/.git && echo '[core]' >mem/.git/config
    out="$(quarantine false 2>&1)"
    assert_missing mem/.git
    assert_contains ".git (name starts with . or -)" "$out"
}

test_memory_repo_files_kept() {
    new_repo mem
    echo '*.tmp' >mem/.gitignore
    echo note >mem/keep.md
    echo 'x filter=evil' >mem/.gitattributes
    out="$(quarantine true "$T/mem/.git" "$T/mem/.gitignore" 2>&1)"
    assert_exists mem/.git/config
    assert_exists mem/.gitignore
    assert_exists mem/keep.md
    assert_missing mem/.gitattributes
    assert_contains ".gitattributes" "$out"
    assert_not_contains ".gitignore" "$out"
}

test_unreadable_folder_reported() {
    mkdir -p mem/locked
    chmod 000 mem/locked
    out="$(quarantine false 2>&1)"
    chmod 755 mem/locked
    assert_contains "Memory check incomplete" "$out"
}

test_read_only_folder_still_moved() {
    mkdir -p mem/.hidden/inner
    echo x >mem/.hidden/inner/f
    chmod 555 mem/.hidden/inner mem/.hidden
    mkdir -p q-other
    out="$(QUARANTINE_ID=7 quarantine false 2>&1)"
    assert_missing mem/.hidden
    assert_contains "-7-after" "$out"
}

test_display_path() {
    mkdir mem && echo x >mem/evil.sh
    out="$(QUARANTINE_DISPLAY=/host/quarantine quarantine false 2>&1)"
    assert_contains "to /host/quarantine/" "$out"
}
