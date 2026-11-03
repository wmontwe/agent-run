# lib/tools.sh (the parts that run without Podman)

test_no_tool_config() {
    mkdir ws
    copy_tool_config "$T/ws" "$T/dest"
    assert_eq none "$TOOL_CONFIG"
}

test_locked_and_unlocked_config() {
    mkdir ws
    echo '[tools]' >ws/mise.toml
    copy_tool_config "$T/ws" "$T/a"
    assert_eq unlocked "$TOOL_CONFIG"
    echo '# lock' >ws/mise.lock
    copy_tool_config "$T/ws" "$T/b"
    assert_eq locked "$TOOL_CONFIG"
    assert_exists "$T/b/mise.toml"
    assert_exists "$T/b/mise.lock"
}

test_lock_alone_is_no_config() {
    mkdir ws
    echo '# lock' >ws/mise.lock
    copy_tool_config "$T/ws" "$T/dest"
    assert_eq none "$TOOL_CONFIG"
}

# The install container gets copies, and never follows links out of the
# workspace: a symlinked mise.toml is left behind.
test_symlinked_config_not_copied() {
    mkdir ws
    echo '[tools]' >elsewhere.toml
    ln -s "$T/elsewhere.toml" ws/mise.toml
    copy_tool_config "$T/ws" "$T/dest"
    assert_eq none "$TOOL_CONFIG"
    assert_missing "$T/dest/mise.toml"
}

test_wrapper_copied() {
    mkdir -p ws/gradle/wrapper
    echo 'distributionUrl=https\://services.gradle.org/distributions/gradle-9.7.0-all.zip' \
        >ws/gradle/wrapper/gradle-wrapper.properties
    echo jar >ws/gradle/wrapper/gradle-wrapper.jar
    copy_tool_config "$T/ws" "$T/dest"
    assert_exists "$T/dest/gradle-wrapper.properties"
    assert_exists "$T/dest/gradle-wrapper.jar"
    assert_eq none "$TOOL_CONFIG"
}

test_symlinked_wrapper_jar_not_copied() {
    mkdir -p ws/gradle/wrapper
    echo jar >elsewhere.jar
    ln -s "$T/elsewhere.jar" ws/gradle/wrapper/gradle-wrapper.jar
    copy_tool_config "$T/ws" "$T/dest"
    assert_missing "$T/dest/gradle-wrapper.jar"
}

test_wrapper_properties_through_symlink_not_copied() {
    mkdir -p ws elsewhere/wrapper
    echo x >elsewhere/wrapper/gradle-wrapper.properties
    ln -s "$T/elsewhere" ws/gradle
    copy_tool_config "$T/ws" "$T/dest"
    assert_missing "$T/dest/gradle-wrapper.properties"
}
