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

# Only the properties: nothing from the wrapper runs.
test_wrapper_properties_copied() {
    mkdir -p ws/gradle/wrapper
    echo 'distributionUrl=https\://services.gradle.org/distributions/gradle-9.7.0-all.zip' \
        >ws/gradle/wrapper/gradle-wrapper.properties
    echo jar >ws/gradle/wrapper/gradle-wrapper.jar
    copy_tool_config "$T/ws" "$T/dest"
    assert_exists "$T/dest/gradle-wrapper.properties"
    assert_missing "$T/dest/gradle-wrapper.jar"
    assert_eq none "$TOOL_CONFIG"
}

test_symlinked_wrapper_properties_not_copied() {
    mkdir -p ws/gradle/wrapper
    echo x >elsewhere.properties
    ln -s "$T/elsewhere.properties" ws/gradle/wrapper/gradle-wrapper.properties
    copy_tool_config "$T/ws" "$T/dest"
    assert_missing "$T/dest/gradle-wrapper.properties"
}

test_wrapper_properties_through_symlink_not_copied() {
    mkdir -p ws elsewhere/wrapper
    echo x >elsewhere/wrapper/gradle-wrapper.properties
    ln -s "$T/elsewhere" ws/gradle
    copy_tool_config "$T/ws" "$T/dest"
    assert_missing "$T/dest/gradle-wrapper.properties"
}

# A mise config is installed only as approved: any change to it, or a missing
# or symlinked record, needs a new approval.
test_tool_config_trust() {
    mkdir ws
    printf '[tools]\njava = "temurin-21"\n' >ws/mise.toml
    echo '# lock' >ws/mise.lock
    copy_tool_config "$T/ws" "$T/a"
    assert_fails tool_config_trusted "$T/a" "$T/trust/record"
    trust_tool_config "$T/a" "$T/trust/record" "$T/ws"
    assert_ok tool_config_trusted "$T/a" "$T/trust/record"
    echo '# changed' >>ws/mise.lock
    copy_tool_config "$T/ws" "$T/b"
    assert_fails tool_config_trusted "$T/b" "$T/trust/record"
    # Same content under another name is another config.
    rm ws/mise.lock && echo '# lock' >ws/mise.lock && mv ws/mise.toml ws/.mise.toml
    copy_tool_config "$T/ws" "$T/c"
    assert_fails tool_config_trusted "$T/c" "$T/trust/record"
    mv trust/record trust/real && ln -s "$T/trust/real" trust/record
    assert_fails tool_config_trusted "$T/a" "$T/trust/record"
}

test_gradle_dist_name() {
    eval "$GRADLE_DIST_FUNCTIONS"
    assert_eq gradle-9.7.0-bin "$(gradle_dist_name https://services.gradle.org/distributions/gradle-9.7.0-bin.zip)"
    assert_eq gradle-9.7.0-rc-1-all "$(gradle_dist_name https://services.gradle.org/distributions/gradle-9.7.0-rc-1-all.zip)"
    assert_eq "" "$(gradle_dist_name https://example.com/distributions/gradle-9.7.0-bin.zip)"
    assert_eq "" "$(gradle_dist_name http://services.gradle.org/distributions/gradle-9.7.0-bin.zip)"
    assert_eq "" "$(gradle_dist_name https://services.gradle.org.example.com/distributions/gradle-9.7.0-bin.zip)"
    assert_eq "" "$(gradle_dist_name https://services.gradle.org/distributions/../x/gradle-9.7.0-bin.zip)"
    assert_eq "" "$(gradle_dist_name https://services.gradle.org/distributions/gradle-9.7.0-bin.zip?x)"
    assert_eq "" "$(gradle_dist_name https://services.gradle.org/distributions-snapshots/gradle-9.7.0-bin.zip)"
}

# The last distributionUrl counts, as Java reads properties; \: and CRLF are
# undone.
test_gradle_dist_url() {
    eval "$GRADLE_DIST_FUNCTIONS"
    printf '%s\r\n' 'distributionUrl=https\://example.com/gradle-1-bin.zip' \
        'distributionUrl=https\://services.gradle.org/distributions/gradle-9.7.0-bin.zip' >p
    assert_eq https://services.gradle.org/distributions/gradle-9.7.0-bin.zip "$(gradle_dist_url p)"
}
