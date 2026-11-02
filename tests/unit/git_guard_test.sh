# lib/git-guard.sh. Git runs with GIT_CONFIG_GLOBAL=/dev/null, so only the
# repository's own config and files set here count.

setup_guard() {
    new_repo ws
    mkdir -p mem tmp outside
    git_guard_init "$T/ws" "$T/mem" false "$T/tmp"
}

guard_ws() { check_git_config 0 "$T/ws"; }

test_clean_repo_passes() {
    setup_guard
    assert_ok guard_ws
}

test_hooks_path_into_workspace() {
    setup_guard
    git -C ws config core.hooksPath .husky
    assert_dies "core.hookspath = .husky" guard_ws
    git -C ws config core.hooksPath "$T/ws/../ws/hooks"
    assert_dies "points into the workspace or memory" guard_ws
}

test_hooks_path_outside_or_in_git_allowed() {
    setup_guard
    git -C ws config core.hooksPath "$T/outside/hooks"
    assert_ok guard_ws
    git -C ws config core.hooksPath .git/hooks-custom
    assert_ok guard_ws
}

test_include_into_workspace() {
    setup_guard
    git -C ws config include.path ../team.gitconfig
    assert_dies "include.path = ../team.gitconfig" guard_ws
}

test_include_nested_through_outside_file() {
    setup_guard
    printf '[include]\n\tpath = %s/ws/x.cfg\n' "$T" >outside/a.cfg
    git -C ws config include.path "$T/outside/a.cfg"
    assert_dies "(from $T/outside/a.cfg" guard_ws
}

test_include_through_symlink() {
    setup_guard
    ln -s "$T/ws/inner.cfg" outside/link.cfg
    git -C ws config include.path "$T/outside/link.cfg"
    assert_dies "include.path = $T/outside/link.cfg" guard_ws
}

test_include_if_variants() {
    setup_guard
    mkdir -p "$HOME/cfg" && touch "$HOME/cfg/w.cfg"
    git -C ws config 'includeIf.gitdir:/x/.path' '~/cfg/w.cfg'
    assert_ok guard_ws
    git -C ws config 'includeIf.onbranch:main.path' sub/my.cfg
    assert_ok guard_ws
    git -C ws config 'includeIf.onbranch:dev.path' ../evil.cfg
    assert_dies "includeif.onbranch:dev.path = ../evil.cfg" guard_ws
}

test_second_include_checked() {
    setup_guard
    git -C ws config --add include.path "$T/outside/ok.cfg"
    git -C ws config --add include.path ../second.cfg
    assert_dies "../second.cfg" guard_ws
}

test_global_scope_checked() {
    setup_guard
    printf '[core]\n\thooksPath = %s/ws/hooks\n' "$T" >global.cfg
    export GIT_CONFIG_GLOBAL="$T/global.cfg"
    assert_dies "(from $T/global.cfg" guard_ws
}

test_malformed_config_fails_closed() {
    setup_guard
    printf '[core\n' >ws/.git/config
    assert_dies "could not read Git configuration" guard_ws
}

test_command_settings_refused() {
    setup_guard
    git -C ws config core.fsmonitor ./tools/fsmon.sh
    assert_dies "runs ./tools/fsmon.sh" guard_ws
    git -C ws config --unset core.fsmonitor
    git -C ws config filter.f.clean 'sh scripts/clean.sh %f'
    assert_dies "runs scripts/clean.sh" guard_ws
    git -C ws config --unset filter.f.clean
    git -C ws config alias.bd "!$T/ws/bin/do"
    assert_dies "runs $T/ws/bin/do" guard_ws
}

test_command_settings_allowed() {
    setup_guard
    git -C ws config core.fsmonitor true
    git -C ws config alias.sv '!git add . && git commit'
    git -C ws config diff.img.textconv '~/bin/exif'
    git -C ws config credential.helper osxkeychain
    assert_ok guard_ws
}

test_workspace_and_memory_cross_checked() {
    setup_guard
    new_repo mem
    git_guard_init "$T/ws" "$T/mem" true "$T/tmp"
    git -C ws config include.path "$T/mem/evil.cfg"
    assert_dies "points into the workspace or memory" guard_ws
    git -C ws config --unset include.path
    git -C mem config core.sshCommand "ssh -F $T/ws/ssh.cfg"
    assert_dies "runs $T/ws/ssh.cfg" check_git_config 0 "$T/mem"
    git -C mem config --unset core.sshCommand
    git -C mem config core.hooksPath hooks
    assert_dies "in $T/mem" check_git_config 0 "$T/mem"
}

test_memory_git_only_trusted_when_repo() {
    setup_guard
    mkdir -p mem/.git
    assert_ok agent_writable "$T/mem/.git/config"
    git_guard_init "$T/ws" "$T/mem" true "$T/tmp"
    assert_dies "" agent_writable "$T/mem/.git/config"
}

test_hooks_path_through_alias_link() {
    setup_guard
    ln -s "$T/ws" aliasws
    ln -s "$T/aliasws/newdir/hooks" outside/hooklink
    git -C ws config core.hooksPath "$T/outside/hooklink"
    assert_dies "points into the workspace or memory" guard_ws
}

test_symlink_loop_fails_closed() {
    setup_guard
    ln -s loop outside/loop
    git -C ws config include.path "$T/outside/loop"
    assert_dies "can't be resolved" guard_ws
}

# Every command-valued setting the guard knows, pointing at a workspace script.
test_all_command_settings_refused() {
    setup_guard
    local kv key
    for kv in core.fsmonitor=./x.sh core.sshCommand=./x.sh core.editor=./x.sh \
        core.pager=./x.sh core.askPass=./x.sh core.gitProxy=./x.sh sequence.editor=./x.sh \
        gpg.program=./x.sh gpg.ssh.program=./x.sh credential.helper=./x.sh \
        credential.https://example.com.helper=./x.sh alias.x='!./x.sh' pager.log=./x.sh \
        diff.d.command=./x.sh diff.d.textconv=./x.sh difftool.d.cmd=./x.sh \
        mergetool.m.path=./x.sh merge.m.driver=./x.sh filter.f.clean=./x.sh \
        filter.f.smudge=./x.sh filter.f.process=./x.sh uploadpack.packObjectsHook=./x.sh \
        sendemail.sendmailCmd=./x.sh diff.external=./x.sh interactive.diffFilter=./x.sh \
        gpg.ssh.defaultKeyCommand=./x.sh core.alternateRefsCommand=./x.sh \
        trailer.sign.cmd=./x.sh trailer.sign.command=./x.sh remote.origin.uploadpack=./x.sh \
        remote.origin.receivepack=./x.sh tar.tgz.command=./x.sh browser.b.cmd=./x.sh \
        man.m.cmd=./x.sh submodule.lib.update='!./x.sh'; do
        key="${kv%%=*}"
        git -C ws config "$key" "${kv#*=}"
        assert_dies "runs ./x.sh" guard_ws
        git -C ws config --unset "$key"
    done
}
