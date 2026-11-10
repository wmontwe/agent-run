# lib/gradle.sh

test_find_gradle_state() {
    mkdir -p ws/app/src/build ws/build-logic/conv ws/lib/build ws/.git/x ws/node_modules/n
    touch ws/settings.gradle.kts ws/build.gradle.kts ws/app/build.gradle.kts \
        ws/build-logic/settings.gradle ws/build-logic/conv/build.gradle \
        ws/lib/build.gradle.kts ws/lib/build.gradle ws/lib/build/build.gradle.kts \
        ws/.git/x/build.gradle.kts ws/node_modules/n/build.gradle.kts \
        ws/app/src/build/notes.txt
    find_gradle_state "$T/ws" "$T"
    assert_eq ".gradle .kotlin app/build build build-logic/.gradle build-logic/.kotlin build-logic/conv/build lib/build" \
        "${GRADLE_STATE[*]}"
}

test_find_gradle_state_none() {
    mkdir ws
    touch ws/package.json
    find_gradle_state "$T/ws" "$T"
    assert_eq 0 "${#GRADLE_STATE[@]}"
}

test_find_gradle_state_refuses_symlink_or_file() {
    mkdir -p ws/app
    touch ws/app/build.gradle.kts
    ln -s /etc ws/app/build
    assert_dies "is a symlink" find_gradle_state "$T/ws" "$T"
    rm ws/app/build
    touch ws/app/build
    assert_dies "is not a directory" find_gradle_state "$T/ws" "$T"
}

test_find_gradle_state_refuses_unsafe_names() {
    mkdir -p "ws/a,b"
    touch "ws/a,b/build.gradle.kts"
    assert_dies "unsupported characters" find_gradle_state "$T/ws" "$T"
}

test_workspace_volume() {
    local a b c
    a="$(workspace_volume claude build /p/one)"
    b="$(workspace_volume claude build /p/two)"
    c="$(workspace_volume claude gradle /p/one)"
    [[ "$a" == agent-run-claude-build-???????????? && "$a" != "$b" ]] || fail "$a $b"
    assert_eq "$a" "$(workspace_volume claude build /p/one)"
    assert_eq "agent-run-claude-gradle-${a##*-}" "$c"
}

test_tool_mounts_gradle_home_per_workspace() {
    tool_mounts claude agent-run-claude-gradle-0123456789ab
    [[ "$TOOL_MOUNTS" == *"src=agent-run-claude-gradle-0123456789ab,dst=/home/node/.gradle"* ]] ||
        fail "$TOOL_MOUNTS"
    [[ "$TOOL_MOUNTS" == *"src=agent-run-claude-mise,dst=/mise,ro=true"* ]] || fail "$TOOL_MOUNTS"
}

# podman-compose splits the mounts with shlex; check they survive it.
test_gradle_state_mounts_quoting() {
    gradle_state_mounts vol "/p/it's a ws" .gradle app/build
    assert_eq "--mount|type=volume,src=vol,dst=/p/it's a ws/.gradle,subpath=.gradle|--mount|type=volume,src=vol,dst=/p/it's a ws/app/build,subpath=app/build" \
        "$(python3 -I -c 'import shlex,sys; print("|".join(shlex.split(sys.argv[1])))' "$GRADLE_STATE_MOUNTS")"
}
