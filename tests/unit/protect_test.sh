# lib/protect.sh

test_require_git_dir() {
    new_repo ok
    assert_ok require_git_dir "$T/ok"
    mkdir none
    assert_dies "is not a Git repository" require_git_dir "$T/none"
    mkdir file && echo 'gitdir: /x' >file/.git
    assert_dies "is not a directory (worktree" require_git_dir "$T/file"
    mkdir link && ln -s "$T/ok/.git" link/.git
    assert_dies "is a symlink" require_git_dir "$T/link"
    mkdir dangling && ln -s /nonexistent dangling/.git
    assert_dies "is a symlink" require_git_dir "$T/dangling"
}

test_check_protectable_dir() {
    assert_ok check_protectable_dir "$T/missing"
    mkdir dir
    assert_ok check_protectable_dir "$T/dir"
    touch file
    assert_dies "is not a directory" check_protectable_dir "$T/file"
    ln -s "$T/dir" link
    assert_dies "is a symlink" check_protectable_dir "$T/link"
}

test_check_no_symlinks() {
    mkdir -p idea/run
    echo x >idea/run/app.xml
    assert_ok check_no_symlinks "$T/idea"
    ln -s ../../elsewhere idea/run/link.xml
    assert_dies "is a symlink" check_no_symlinks "$T/idea"
    rm idea/run/link.xml && ln -s /nonexistent idea/dangling
    assert_dies "is a symlink" check_no_symlinks "$T/idea"
}

test_detect_memory_repo_plain_folder() {
    mkdir mem
    detect_memory_repo "$T/mem" "$T/marker"
    assert_eq false "$MEM_REPO"
    assert_eq 0 "${#MEM_PROTECTED[@]}"
}

test_detect_memory_repo_trusted() {
    new_repo mem
    echo '*.tmp' >mem/.gitignore
    trust_memory_repo "$T/mem" "$T/marker"
    detect_memory_repo "$T/mem" "$T/marker"
    assert_eq true "$MEM_REPO"
    assert_eq "$T/mem/.git $T/mem/.gitignore" "${MEM_PROTECTED[*]}"
}

# The review's attack: an agent creates .git in a memory folder that isn't a
# repository, with a fsmonitor pointing at its own script inside it.
test_agent_made_memory_git_refused() {
    mkdir -p mem/.git
    printf '[core]\n\tfsmonitor = %s/mem/.git/x.sh\n' "$T" >mem/.git/config
    assert_dies "isn't recorded as your memory repository" detect_memory_repo "$T/mem" "$T/marker"
}

test_replaced_memory_git_refused() {
    new_repo mem
    trust_memory_repo "$T/mem" "$T/marker"
    mv mem/.git old-git && mkdir mem/.git
    assert_dies "is not the memory repository recorded" detect_memory_repo "$T/mem" "$T/marker"
}

test_recorded_memory_git_missing_refused() {
    new_repo mem
    trust_memory_repo "$T/mem" "$T/marker"
    rm -rf mem/.git
    assert_dies "is missing" detect_memory_repo "$T/mem" "$T/marker"
}

test_trust_memory_repo_needs_real_git() {
    mkdir mem
    assert_dies "is not a Git repository" trust_memory_repo "$T/mem" "$T/marker"
    ln -s /tmp mem/.git
    assert_dies "is a symlink" trust_memory_repo "$T/mem" "$T/marker"
}

test_detect_memory_repo_refusals() {
    mkdir a && ln -s /tmp a/.git
    assert_dies "is a symlink" detect_memory_repo "$T/a" "$T/m1"
    mkdir b && touch b/.git
    assert_dies "is not a directory" detect_memory_repo "$T/b" "$T/m2"
    new_repo c && trust_memory_repo "$T/c" "$T/m3" && ln -s /etc/hosts c/.gitignore
    assert_dies ".gitignore is a symlink" detect_memory_repo "$T/c" "$T/m3"
    new_repo d && trust_memory_repo "$T/d" "$T/m4" && mkdir d/.gitattributes
    assert_dies ".gitattributes is not a regular file" detect_memory_repo "$T/d" "$T/m4"
}

# check_hard_links with the paths bin/agent protects in a workspace and memory.
hard_links() {
    local protected=("$T/ws/.git")
    [[ -d "$T/ws/.idea" ]] && protected+=("$T/ws/.idea")
    [[ -d "$T/ws/.signing" ]] && protected+=("$T/ws/.signing")
    [[ -d "$T/mem/.git" ]] && protected+=("$T/mem/.git")
    [[ -f "$T/mem/.gitignore" ]] && protected+=("$T/mem/.gitignore")
    mkdir -p "$T/tmp" "$T/agent-home"
    check_hard_links "$T/tmp" 3 "$T/ws" "$T/mem" "$T/agent-home" "${protected[@]}"
}

setup_hard_links() {
    new_repo ws
    mkdir -p mem ws/.idea ws/.signing
    echo idea >ws/.idea/guard.xml
    echo key >ws/.signing/release.jks
}

test_hard_links_none() {
    setup_hard_links
    assert_ok hard_links
}

test_hard_links_idea_into_workspace() {
    setup_hard_links
    ln ws/.idea/guard.xml ws/alias.xml
    assert_dies ".idea/guard.xml is hard-linked" hard_links
}

test_hard_links_workspace_file_into_idea() {
    setup_hard_links
    echo outside >ws/out.txt
    ln ws/out.txt ws/.idea/linked.xml
    assert_dies "is hard-linked" hard_links
}

test_hard_links_git_config_and_signing() {
    setup_hard_links
    ln ws/.git/config ws/cfg-alias
    assert_dies ".git/config is hard-linked" hard_links
    rm ws/cfg-alias
    ln ws/.signing/release.jks ws/key-alias
    assert_dies ".signing/release.jks is hard-linked" hard_links
}

test_hard_links_memory_gitignore() {
    setup_hard_links
    new_repo mem
    echo x >mem/.gitignore
    ln mem/.gitignore mem/notes.md
    assert_dies "memory" hard_links
}

test_hard_links_into_agent_home() {
    setup_hard_links
    mkdir -p agent-home
    ln ws/.signing/release.jks agent-home/stolen.jks
    assert_dies ".signing/release.jks is hard-linked" hard_links
}

test_hard_links_outside_mounts_allowed() {
    setup_hard_links
    ln ws/.idea/guard.xml elsewhere.xml
    assert_ok hard_links
}

test_hard_links_local_clone_allowed() {
    new_repo src
    echo f >src/f && g -C src add f && g -C src commit -qm init
    git clone -q --local src ws
    mkdir mem
    [[ -n "$(find ws/.git -type f -links +1 -print -quit)" ]] || fail "expected linked objects"
    assert_ok hard_links
}
