# Engram integration. An engram context repository (e.g. engram-work) keeps a
# project's agent context outside the project: the project's top-level
# AGENTS.md, CLAUDE.md, .memory, .claude, .agents, ... are relative links
# `../<repo>/projects/<project>/<name>`. A project that commits its own
# AGENTS.md has links for the rest only. The container sees the workspace at
# its host path; mounting the package where the links lead,
# `<workspace's parent>/<repo>/projects/<project>`, makes them work unchanged. The package is read-only except its
# .memory; the package's skills link into `<repo>/engram-core/.agents/skills`,
# which is mounted read-only too. The repository's .git, the engram script and
# engram-core's scripts are never mounted: notes are saved on the host.

# detect_engram WS: if any top-level entry of WS is engram's link (named like
# its target), validates the context repository and sets ENGRAM=true,
# ENGRAM_ROOT (canonical repository), ENGRAM_AT (its container path, where
# the links lead),
# ENGRAM_PROJECT, ENGRAM_PACKAGE (canonical package) and, when they exist,
# ENGRAM_MEMORY (the package's .memory) and ENGRAM_SKILLS (engram-core's
# skills). Without such a link it sets ENGRAM=false. Links in engram's form
# that disagree on repository or project, or that don't lead to a valid
# context repository, are refused rather than half-mounted.
detect_engram() {
    local ws="$1" entry name link dir="" project="" first="" d p rest candidate
    ENGRAM=false ENGRAM_ROOT="" ENGRAM_AT="" ENGRAM_PROJECT=""
    ENGRAM_PACKAGE="" ENGRAM_MEMORY="" ENGRAM_SKILLS=""
    for entry in "$ws"/* "$ws"/.*; do
        [[ -L "$entry" ]] || continue
        name="${entry##*/}"
        link="$(readlink -- "$entry")"
        [[ "$link" == ../*/projects/*/"$name" ]] || continue
        rest="${link#../}"
        d="${rest%%/*}"
        rest="${rest#"$d"/projects/}"
        p="${rest%/"$name"}"
        for candidate in "$d" "$p"; do
            case "$candidate" in
                "" | . | .. | *[!A-Za-z0-9._-]*)
                    die "$entry links to $link, which looks like engram's but isn't a form agent-run supports" ;;
            esac
        done
        if [[ -z "$first" ]]; then
            first="$entry" dir="$d" project="$p"
        elif [[ "$d/$p" != "$dir/$project" ]]; then
            die "$first and $entry link into different engram projects ($dir/$project, $d/$p)"
        fi
    done
    [[ -n "$first" ]] || return 0

    ENGRAM_ROOT="$(canonical "${ws%/*}/$dir")" ||
        die "$first links into ../$dir, but ${ws%/*}/$dir doesn't exist"
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
    ENGRAM=true ENGRAM_AT="${ws%/*}/$dir" ENGRAM_PROJECT="$project"
}
