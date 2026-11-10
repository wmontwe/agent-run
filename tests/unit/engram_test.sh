# lib/engram.sh, and the engram cases of the guard, hard links, quarantine
# and report.

# setup_engram [REPO]: a context repository $T/REPO (default engram-work) with
# project p, and a workspace $T/p linked to it the way `engram setup` does.
setup_engram() {
    local repo="${1:-engram-work}"
    mkdir -p "$repo/projects/p/.memory/plans" "$repo/engram-core/.agents/skills/shared"
    echo '# context' >"$repo/AGENTS.md"
    printf '#!/bin/sh\n' >"$repo/engram"
    echo '# project' >"$repo/projects/p/AGENTS.md"
    : >"$repo/projects/p/.memory/plans/.gitkeep"
    new_repo p
    ln -s "../$repo/projects/p/AGENTS.md" p/AGENTS.md
    ln -s "../$repo/projects/p/.memory" p/.memory
}

test_plain_project_is_not_engram() {
    new_repo p
    echo '# own' >p/AGENTS.md
    detect_engram "$T/p"
    assert_eq false "$ENGRAM"
    ln -sf ../shared/AGENTS.md p/AGENTS.md
    detect_engram "$T/p"
    assert_eq false "$ENGRAM"
}

test_engram_detected() {
    setup_engram
    detect_engram "$T/p"
    assert_eq true "$ENGRAM"
    assert_eq "$T/engram-work" "$ENGRAM_ROOT"
    assert_eq "$T/engram-work" "$ENGRAM_AT"
    assert_eq p "$ENGRAM_PROJECT"
    assert_eq "$T/engram-work/projects/p" "$ENGRAM_PACKAGE"
    assert_eq "$T/engram-work/projects/p/.memory" "$ENGRAM_MEMORY"
    assert_eq "$T/engram-work/engram-core/.agents/skills" "$ENGRAM_SKILLS"
}

# A project that commits its own AGENTS.md: engram links the other files.
test_engram_detected_without_agents_link() {
    setup_engram
    rm p/AGENTS.md
    echo '# own' >p/AGENTS.md
    echo '# claude' >engram-work/projects/p/CLAUDE.md
    ln -s ../engram-work/projects/p/CLAUDE.md p/CLAUDE.md
    ln -s app/src p/source
    detect_engram "$T/p"
    assert_eq true "$ENGRAM"
    assert_eq p "$ENGRAM_PROJECT"
    assert_eq "$T/engram-work/projects/p/.memory" "$ENGRAM_MEMORY"
}

test_links_into_different_projects_refused() {
    setup_engram
    mkdir -p engram-work/projects/q
    echo '# q' >engram-work/projects/q/AGENTS.md
    echo '# q' >engram-work/projects/q/CLAUDE.md
    ln -s ../engram-work/projects/q/CLAUDE.md p/CLAUDE.md
    assert_dies "link into different engram projects" detect_engram "$T/p"
}

test_other_context_repo_name() {
    setup_engram engram-personal
    detect_engram "$T/p"
    assert_eq "$T/engram-personal" "$ENGRAM_AT"
}

test_missing_memory_and_skills_optional() {
    setup_engram
    rm -rf engram-work/projects/p/.memory engram-work/engram-core/.agents
    detect_engram "$T/p"
    assert_eq true "$ENGRAM"
    assert_eq "" "$ENGRAM_MEMORY"
    assert_eq "" "$ENGRAM_SKILLS"
}

test_refusals() {
    setup_engram
    rm p/.memory
    ln -sfn '../engram-work/projects/a b/AGENTS.md' p/AGENTS.md
    assert_dies "isn't a form agent-run supports" detect_engram "$T/p"
    ln -sfn ../missing/projects/p/AGENTS.md p/AGENTS.md
    assert_dies "doesn't exist" detect_engram "$T/p"
    ln -sfn ../engram-work/projects/p/AGENTS.md p/AGENTS.md
    rm engram-work/engram
    assert_dies "is not an engram context repository" detect_engram "$T/p"
}

test_symlinked_package_or_memory_refused() {
    setup_engram
    mv engram-work/projects/p real-p
    ln -s "$T/real-p" engram-work/projects/p
    assert_dies "is not a plain directory" detect_engram "$T/p"
    rm engram-work/projects/p && mv real-p engram-work/projects/p
    mv engram-work/projects/p/.memory real-mem
    ln -s "$T/real-mem" engram-work/projects/p/.memory
    assert_dies ".memory is a symlink" detect_engram "$T/p"
}

test_guard_treats_engram_memory_as_writable() {
    setup_engram
    mkdir -p mem tmp
    git_guard_init "$T/p" "$T/mem" false "$T/tmp" "$T/engram-work/projects/p/.memory"
    new_repo engram-work
    git -C engram-work config core.fsmonitor "$T/engram-work/projects/p/.memory/x.sh"
    assert_dies "runs $T/engram-work/projects/p/.memory/x.sh" check_git_config 0 "$T/engram-work"
    git -C engram-work config --unset core.fsmonitor
    git -C engram-work config include.path ../projects/p/.memory/x.cfg
    assert_dies "points into the workspace or memory" check_git_config 0 "$T/engram-work"
}

test_hard_links_engram_package_and_memory() {
    setup_engram
    mkdir -p mem home tmp
    local writable=("$T/p" "$T/mem" "$T/home" "$T/engram-work/projects/p/.memory")
    local protected=("$T/p/.git" "$T/engram-work/projects/p" "$T/engram-work/engram-core/.agents/skills")
    # Notes inside the writable .memory may share inodes with each other.
    echo n >engram-work/projects/p/.memory/a.md
    ln engram-work/projects/p/.memory/a.md engram-work/projects/p/.memory/b.md
    assert_ok check_hard_links "$T/tmp" 4 0 "${writable[@]}" "${protected[@]}"
    # The read-only package linked into its own writable .memory is not.
    ln engram-work/projects/p/AGENTS.md engram-work/projects/p/.memory/agents.md
    assert_dies "projects/p/AGENTS.md is hard-linked" check_hard_links "$T/tmp" 4 0 "${writable[@]}" "${protected[@]}"
}

test_empty_gitkeep_kept_others_quarantined() {
    mkdir -p mem/plans tmp
    : >mem/plans/.gitkeep
    echo x >mem/.gitkeep
    out="$(quarantine_memory after "$T/mem" "$T/q" "$T/tmp" false 2>&1)"
    assert_exists mem/plans/.gitkeep
    assert_missing mem/.gitkeep
    assert_contains ".gitkeep (name starts with . or -)" "$out"
}

test_report_engram() {
    out="$(report_engram "$T/engram-work" p 2>&1)"
    assert_contains "status --untracked-files=all --ignored -- projects/p/.memory" "$out"
    assert_contains "$T/engram-work/engram save p" "$out"
}

test_instruction_links_are_sensitive() {
    assert_ok is_sensitive "$T/p/AGENTS.md"
    assert_ok is_sensitive "$T/p/CLAUDE.md"
    assert_ok is_sensitive "$T/p/.agents/skills/x/SKILL.md"
    assert_ok is_sensitive "$T/p/.junie"
}
