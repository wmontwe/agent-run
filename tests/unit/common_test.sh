# lib/common.sh

test_canonical_resolves_symlinks() {
    mkdir -p real && ln -s real link
    assert_eq "$T/real" "$(canonical link)"
}

test_canonical_fails_for_missing() {
    assert_dies "" canonical "$T/missing"
}

test_overlaps() {
    assert_ok overlaps /a/b /a/b
    assert_ok overlaps /a /a/b
    assert_ok overlaps /a/b /a
    assert_ok overlaps / /anything
    assert_dies "" overlaps /a/b /a/bc
    assert_dies "" overlaps /a/b /a/c
}

test_check_path_chars() {
    assert_ok check_path_chars workspace "/ok/path with space"
    for bad in "/a:b" "/a,b" "/a*b" "/a?b" "/a[b" "/a]b" '/a\b' $'/a\nb'; do
        assert_dies "workspace path contains an unsupported character" check_path_chars workspace "$bad"
    done
}

test_resolve_path_relative_and_missing_tail() {
    mkdir -p repo
    assert_eq "$T/repo/hooks/pre-commit" "$(resolve_path hooks/pre-commit "$T/repo")"
}

test_resolve_path_home_and_dotdot() {
    mkdir -p "$HOME/cfg" repo
    assert_eq "$HOME/cfg/x" "$(resolve_path "~/cfg/x" "$T/repo")"
    assert_eq "$T/repo/hooks" "$(resolve_path "$T/repo/../repo/hooks" /)"
}

test_resolve_path_follows_symlinks() {
    mkdir -p target outside
    ln -s "$T/target" outside/dirlink
    ln -s "$T/target/file.cfg" outside/filelink
    assert_eq "$T/target/x" "$(resolve_path "$T/outside/dirlink/x" /)"
    assert_eq "$T/target/file.cfg" "$(resolve_path "$T/outside/filelink" /)"
}

# Review finding: a link to a symlinked location whose last part doesn't exist
# yet must still resolve to the physical path.
test_resolve_path_link_to_alias_with_missing_tail() {
    mkdir -p realws outside
    ln -s "$T/realws" aliasws
    ln -s "$T/aliasws/newdir/hooks" outside/hooklink
    assert_eq "$T/realws/newdir/hooks" "$(resolve_path "$T/outside/hooklink" /)"
}

test_resolve_path_relative_link_chain() {
    mkdir -p a/b target
    ln -s ../../target a/b/up
    ln -s a/b/up/x chain
    assert_eq "$T/target/x" "$(resolve_path "$T/chain" /)"
}

test_resolve_path_symlink_loop_fails() {
    ln -s loop loop
    ln -s two one && ln -s one two
    assert_dies "" resolve_path "$T/loop" /
    assert_dies "" resolve_path "$T/one/x" /
}

test_git_excludes_file_from_config() {
    echo '*.log' >ignores
    printf '[core]\n\texcludesFile = %s\n' "$T/ignores" >gitconfig
    assert_eq "$T/ignores" "$(GIT_CONFIG_GLOBAL="$T/gitconfig" git_excludes_file)"
}

test_git_excludes_file_default_location() {
    mkdir -p "$HOME/.config/git"
    echo '*.log' >"$HOME/.config/git/ignore"
    assert_eq "$HOME/.config/git/ignore" "$(XDG_CONFIG_HOME= git_excludes_file)"
}

test_git_excludes_file_missing() {
    printf '[core]\n\texcludesFile = %s\n' "$T/nowhere" >gitconfig
    assert_eq "" "$(GIT_CONFIG_GLOBAL="$T/gitconfig" git_excludes_file)"
}

test_check_container_path() {
    assert_ok check_container_path workspace /Users/me/src/app
    assert_ok check_container_path workspace /private/var/folders/x/app
    assert_ok check_container_path workspace /Volumes/work/app
    for path in /opt/src/app /usr/local/src/app /home/me/app /tmp/app /mise/x; do
        assert_dies "can't appear at the same path" check_container_path workspace "$path"
    done
}
