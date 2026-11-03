# Checks for the paths bin/agent protects with read-only or hiding mounts:
# Git metadata, the protected workspace folders and memory's own Git files.

# require_git_dir DIR: DIR/.git must be a plain directory. Checks -L first,
# since -d follows links.
require_git_dir() {
    if [[ -L "$1/.git" ]]; then
        die "$1/.git is a symlink; only a plain .git directory is supported"
    elif [[ ! -e "$1/.git" ]]; then
        die "$1 is not a Git repository (no .git directory); run 'git init' first"
    elif [[ ! -d "$1/.git" ]]; then
        die "$1/.git is not a directory (worktree or submodule checkout?); not supported"
    fi
}

# check_protectable_dir PATH: an existing PATH must be a real directory, so
# a mount over it lands where intended. A missing one is fine.
check_protectable_dir() {
    if [[ -L "$1" ]]; then
        die "$1 is a symlink; it can't be protected from the agent"
    elif [[ -e "$1" && ! -d "$1" ]]; then
        die "$1 is not a directory; it can't be protected from the agent"
    fi
}

# check_protectable_file PATH: like check_protectable_dir, for a file.
check_protectable_file() {
    if [[ -L "$1" ]]; then
        die "$1 is a symlink; it can't be protected from the agent"
    elif [[ -e "$1" && ! -f "$1" ]]; then
        die "$1 is not a regular file; it can't be protected from the agent"
    fi
}

# check_no_symlinks DIR: a symlink inside a read-only directory would leave
# its target, possibly in the writable workspace, editable through it.
check_no_symlinks() {
    local link
    link="$(find -P "$1" -type l -print -quit)" || die "could not check $1 for symlinks"
    [[ -z "$link" ]] ||
        die "$(printf '%q' "$link") is a symlink; $1 can't be made read-only safely"
}

# git_dir_id DIR: prints DIR's device and inode, which identify a directory
# even if it is replaced by another with the same name.
git_dir_id() {
    stat -f '%d:%i' "$1"
}

# detect_memory_repo MEM MARKER: memory may be a Git repository, committed on
# the host. Its .git is only trusted if its identity matches the one recorded
# in MARKER (outside every mount) by `agent trust-memory`: an agent can create
# a .git in a memory folder that isn't a repository, and that one must never
# be taken for the user's. Like the workspace's, a trusted .git must be a real
# directory, and memory's own .gitignore and .gitattributes are protected too.
# Sets MEM_REPO (true or false) and MEM_PROTECTED (paths to mount read-only).
detect_memory_repo() {
    local mem="$1" marker="$2" file id
    MEM_REPO=false
    MEM_PROTECTED=()
    if [[ -L "$mem/.git" ]]; then
        die "$mem/.git is a symlink; only a plain .git directory is supported"
    elif [[ -d "$mem/.git" ]]; then
        [[ -f "$marker" ]] ||
            die "$mem/.git exists but isn't recorded as your memory repository; an agent may have created it. Inspect it, and if you created it, run: agent trust-memory"
        id="$(git_dir_id "$mem/.git")" || die "could not identify $mem/.git"
        [[ "$id" == "$(cat "$marker")" ]] ||
            die "$mem/.git is not the memory repository recorded in $marker; an agent may have replaced it. Inspect it, and if it's yours, run: agent trust-memory"
        MEM_REPO=true
        MEM_PROTECTED=("$mem/.git")
        for file in .gitignore .gitattributes; do
            if [[ -L "$mem/$file" ]]; then
                die "$mem/$file is a symlink; it can't be protected from the agent"
            elif [[ -f "$mem/$file" ]]; then
                MEM_PROTECTED+=("$mem/$file")
            elif [[ -e "$mem/$file" ]]; then
                die "$mem/$file is not a regular file; it can't be protected from the agent"
            fi
        done
    elif [[ -e "$mem/.git" ]]; then
        die "$mem/.git is not a directory; only a plain .git directory is supported"
    elif [[ -f "$marker" ]]; then
        die "the memory repository recorded in $marker is missing; restore $mem/.git, or remove $marker if memory is no longer a repository"
    fi
}

# trust_memory_repo MEM MARKER: records MEM's current .git as the user's.
trust_memory_repo() {
    require_git_dir "$1"
    git_dir_id "$1/.git" >"$2" || die "could not record $1/.git"
}

# check_hard_links TMP COUNT WRITABLE... PROTECTED...: a protected file that
# shares its inode (a hard link) with a file in a writable mount (the
# workspace, memory, the agent's home) could be changed, or read, through that
# other name, whatever its own mount says. COUNT is the number of WRITABLE
# folders that follow. Agents can't create such links across mounts (EXDEV),
# so checking the existing ones before launch is enough. Links to files
# outside the mounts, like a local clone's Git objects, are fine. PROTECTED
# paths inside a writable folder are left out of its scan, and writable
# folders inside a protected one (engram's .memory in its read-only package)
# are left out of the protected scan.
check_hard_links() {
    local tmp="$1" count="$2" root path shared name
    shift 2
    local writable=("${@:1:$count}")
    shift "$count"
    local skip=()
    for root in "${writable[@]}"; do
        for path in "$@"; do
            [[ "$root" == "$path"/* ]] && skip+=(-path "$root" -prune -o)
        done
    done
    find -P "$@" ${skip[@]+"${skip[@]}"} -type f -links +1 -exec stat -f '%d:%i' {} + \
        >"$tmp/protected-inodes" || die "could not check protected files for hard links"
    [[ -s "$tmp/protected-inodes" ]] || return 0
    : >"$tmp/writable-inodes"
    for root in "${writable[@]}"; do
        local prune=()
        for path in "$@"; do
            [[ "$path" == "$root"/* ]] && prune+=(-path "$path" -prune -o)
        done
        find -P "$root" ${prune[@]+"${prune[@]}"} -type f -links +1 -exec stat -f '%d:%i' {} + \
            >>"$tmp/writable-inodes" || die "could not check $root for hard links"
    done
    shared="$(sort -u "$tmp/protected-inodes" | comm -12 - <(sort -u "$tmp/writable-inodes") | head -1)"
    [[ -n "$shared" ]] || return 0
    name="$(find -P "$@" ${skip[@]+"${skip[@]}"} -type f -links +1 -exec stat -f '%d:%i %N' {} + |
        grep -m1 "^$shared " | cut -d' ' -f2-)"
    die "$(printf '%q' "$name") is hard-linked to a file in a writable folder (workspace, memory or the agent's home), so an agent could change or read it there; replace the link with a copy"
}
