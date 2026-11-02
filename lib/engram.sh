# Engram integration. An engram context repository (e.g. engram-work) keeps a
# project's agent context outside the project: the project's AGENTS.md is a
# relative link `../<repo>/projects/<project>/AGENTS.md`, and so are its other
# overlays (.memory, .claude, .agents, ...). Mounting the package at the path
# those links reach from /workspace, `/<repo>/projects/<project>`, makes them
# work unchanged in the container. The package is read-only except its
# .memory; the package's skills link into `<repo>/engram-core/.agents/skills`,
# which is mounted read-only too. The repository's .git, the engram script and
# engram-core's scripts are never mounted: notes are saved on the host.

# Top-level container paths an engram repository may not be mounted over.
ENGRAM_RESERVED=" bin boot dev etc home lib lib32 lib64 libx32 media mnt opt proc root run sbin srv sys tmp usr var workspace memory knowledge "

# detect_engram WS: if WS/AGENTS.md is engram's link, validates the context
# repository and sets ENGRAM=true, ENGRAM_ROOT (canonical repository),
# ENGRAM_DIR (its container path below /), ENGRAM_PROJECT, ENGRAM_PACKAGE
# (canonical package) and, when they exist, ENGRAM_MEMORY (the package's
# .memory) and ENGRAM_SKILLS (engram-core's skills). Any other AGENTS.md,
# file or link, sets ENGRAM=false. A link in engram's form that doesn't lead
# to a valid context repository is refused rather than half-mounted.
detect_engram() {
    local ws="$1" link dir project rest candidate
    ENGRAM=false ENGRAM_ROOT="" ENGRAM_DIR="" ENGRAM_PROJECT=""
    ENGRAM_PACKAGE="" ENGRAM_MEMORY="" ENGRAM_SKILLS=""
    [[ -L "$ws/AGENTS.md" ]] || return 0
    link="$(readlink -- "$ws/AGENTS.md")"
    [[ "$link" == ../*/projects/*/AGENTS.md ]] || return 0
    rest="${link#../}"
    dir="${rest%%/*}"
    rest="${rest#"$dir"/projects/}"
    project="${rest%/AGENTS.md}"
    for candidate in "$dir" "$project"; do
        case "$candidate" in
            "" | . | .. | *[!A-Za-z0-9._-]*)
                die "$ws/AGENTS.md links to $link, which looks like engram's but isn't a form agent-run supports" ;;
        esac
    done
    [[ "$ENGRAM_RESERVED" != *" $dir "* ]] ||
        die "$ws/AGENTS.md links into ../$dir, which can't be mounted at /$dir in the container"

    ENGRAM_ROOT="$(canonical "${ws%/*}/$dir")" ||
        die "$ws/AGENTS.md links to $link, but ${ws%/*}/$dir doesn't exist"
    check_path_chars engram "$ENGRAM_ROOT"
    [[ -f "$ENGRAM_ROOT/AGENTS.md" && -d "$ENGRAM_ROOT/projects" &&
        -f "$ENGRAM_ROOT/engram" && -d "$ENGRAM_ROOT/engram-core" ]] ||
        die "$ENGRAM_ROOT is not an engram context repository (AGENTS.md, projects/, engram, engram-core/)"
    # The package and its .memory must be real directories where the links
    # say, not symlinks to somewhere else.
    ENGRAM_PACKAGE="$ENGRAM_ROOT/projects/$project"
    [[ -d "$ENGRAM_PACKAGE" && ! -L "$ENGRAM_PACKAGE" ]] ||
        die "$ENGRAM_PACKAGE is not a plain directory"
    [[ -f "$ENGRAM_PACKAGE/AGENTS.md" && ! -L "$ENGRAM_PACKAGE/AGENTS.md" ]] ||
        die "$ENGRAM_PACKAGE/AGENTS.md is not a plain file"
    if [[ -L "$ENGRAM_PACKAGE/.memory" ]]; then
        die "$ENGRAM_PACKAGE/.memory is a symlink; it can't be mounted safely"
    elif [[ -d "$ENGRAM_PACKAGE/.memory" ]]; then
        ENGRAM_MEMORY="$ENGRAM_PACKAGE/.memory"
    elif [[ -e "$ENGRAM_PACKAGE/.memory" ]]; then
        die "$ENGRAM_PACKAGE/.memory is not a directory"
    fi
    if [[ -d "$ENGRAM_ROOT/engram-core/.agents/skills" && ! -L "$ENGRAM_ROOT/engram-core/.agents/skills" &&
          ! -L "$ENGRAM_ROOT/engram-core/.agents" ]]; then
        ENGRAM_SKILLS="$ENGRAM_ROOT/engram-core/.agents/skills"
    fi
    ENGRAM=true ENGRAM_DIR="$dir" ENGRAM_PROJECT="$project"
}
