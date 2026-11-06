# lib/github.sh

test_github_repos_accepts_list() {
    printf '# comment\n\nowner/repo\nowner-2/repo.name_x\nlast/no-newline' >repos
    check_github_repos repos
}

test_github_repos_refuses_bad_lines() {
    local bad
    for bad in 'repo' 'a/b/c' '../x/y' 'a/..' 'a/b c' 'a/b;' '.*/.*' ' a/b'; do
        printf '%s\n' "$bad" >repos
        assert_dies "is not owner/repo" check_github_repos repos
    done
}

test_github_repos_refuses_symlink() {
    echo a/b >real
    ln -s real repos
    assert_dies "is a symlink" check_github_repos repos
}
