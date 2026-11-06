# The list of repositories the GitHub relay may read.

# check_github_repos FILE: FILE holds one owner/repo per line, plus comments
# and blank lines. The relay ignores anything else, so a mistyped line would
# only show up as a refused request; refuse it here instead. A symlink could
# point into a folder agents can write to.
check_github_repos() {
    local line pattern='^[A-Za-z0-9_-][A-Za-z0-9._-]*/[A-Za-z0-9._-]+$'
    check_protectable_file "$1"
    while IFS= read -r line || [[ -n "$line" ]]; do
        [[ -z "$line" || "$line" == \#* ]] && continue
        [[ "$line" =~ $pattern && "$line" != *..* ]] ||
            die "$1: $(printf '%q' "$line") is not owner/repo"
    done <"$1"
}
