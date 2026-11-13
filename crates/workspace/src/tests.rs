use super::*;
use std::os::unix::fs::symlink;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// A fresh folder under the system's temp folder, canonical like a real checkout.
fn scratch() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "workspace-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

fn checkout() -> PathBuf {
    let root = scratch().join("checkout");
    for dir in [
        ".git",
        "app/src",
        "build",
        ".gradle",
        "app/build",
        "build-logic/build",
        "build-logic/.kotlin",
        "lib/nested/build",
        "gradle/wrapper",
        ".signing",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("settings.gradle.kts"), "").unwrap();
    fs::write(root.join("build-logic/settings.gradle.kts"), "").unwrap();
    root
}

fn rules<'a>(mask: &'a [String], read_only: &'a [String], private: &'a [String]) -> Rules<'a> {
    Rules {
        mask,
        read_only,
        private,
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

fn lock(path: &Path) {
    assert!(
        Command::new("chflags")
            .args(["-h", "uchg"])
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

/// Locked links block removing their folder; unlock them first.
fn unlock_all(dir: &Path) {
    let _ = Command::new("chflags")
        .args(["-R", "-h", "nouchg"])
        .arg(dir)
        .status();
}

#[test]
fn finds_settings_roots_host_state_and_safety_nets() {
    let root = checkout();
    let inspection = inspect(&root, &Rules::default()).unwrap();
    assert_eq!(inspection.settings_roots, ["", "build-logic"]);
    assert_eq!(inspection.safety_net, ["build", "build-logic/build"]);
    assert_eq!(
        inspection.host_state,
        [
            ".gradle",
            "app/build",
            "build-logic/.kotlin",
            "lib/nested/build"
        ]
    );
}

#[test]
fn sorts_rule_folders_and_skips_them_in_the_walk() {
    let root = checkout();
    fs::create_dir_all(root.join("target/debug/build")).unwrap();
    let (mask, read_only, private) = (
        strings(&[".signing", "target", ".idea"]),
        strings(&[".git", "gradle/wrapper"]),
        strings(&["kotlin-js-store"]),
    );
    let inspection = inspect(&root, &rules(&mask, &read_only, &private)).unwrap();
    assert_eq!(inspection.mask, [".signing", "target"]);
    assert_eq!(inspection.read_only, [".git", "gradle/wrapper"]);
    assert_eq!(inspection.absent, [".idea", "kotlin-js-store"]);
    // Inside a masked folder there's nothing to mask separately.
    assert!(
        !inspection
            .host_state
            .iter()
            .any(|s| s.starts_with("target"))
    );
}

#[test]
fn refuses_links_and_files_where_folders_are_protected() {
    let root = checkout();
    let elsewhere = scratch();
    fs::rename(root.join("gradle"), elsewhere.join("gradle")).unwrap();
    symlink(elsewhere.join("gradle"), root.join("gradle")).unwrap();
    let read_only = strings(&["gradle/wrapper"]);
    let e = inspect(&root, &rules(&[], &read_only, &[])).unwrap_err();
    assert!(e.0.contains("gradle is a symlink"), "{e}");

    let root = checkout();
    fs::write(root.join(".idea"), "").unwrap();
    let mask = strings(&[".idea"]);
    assert!(
        inspect(&root, &rules(&mask, &[], &[]))
            .unwrap_err()
            .0
            .contains("not a folder")
    );
}

#[test]
fn refuses_gradle_state_that_is_a_link_or_file() {
    let root = checkout();
    fs::remove_dir(root.join("app/build")).unwrap();
    symlink(scratch(), root.join("app/build")).unwrap();
    assert!(
        inspect(&root, &Rules::default())
            .unwrap_err()
            .0
            .contains("app/build must be a folder")
    );

    let root = checkout();
    fs::write(root.join("lib/.kotlin"), "").unwrap();
    assert!(
        inspect(&root, &Rules::default())
            .unwrap_err()
            .0
            .contains("lib/.kotlin")
    );
}

#[test]
fn does_not_follow_symlinked_folders() {
    let root = checkout();
    let outside = scratch();
    fs::create_dir_all(outside.join("build")).unwrap();
    symlink(&outside, root.join("linked")).unwrap();
    assert!(
        !inspect(&root, &Rules::default())
            .unwrap()
            .host_state
            .iter()
            .any(|s| s.starts_with("linked"))
    );
}

#[test]
fn requires_a_real_git_folder() {
    let root = checkout();
    fs::remove_dir(root.join(".git")).unwrap();
    fs::write(root.join(".git"), "gitdir: elsewhere").unwrap();
    assert!(
        inspect(&root, &Rules::default())
            .unwrap_err()
            .0
            .contains("not a folder")
    );
}

#[test]
fn checks_intellij_reload_setting() {
    let root = checkout();
    assert!(check_ide(&root).is_ok(), "no .idea, nothing to check");
    fs::create_dir(root.join(".idea")).unwrap();
    assert!(
        check_ide(&root).is_err(),
        "default reloads on external changes"
    );
    let xml = |value: &str| {
        format!(
            "<project><component name=\"AutoImportSettings\">\n<option name=\"autoReloadType\" value=\"{value}\" />\n</component></project>"
        )
    };
    fs::write(root.join(".idea/workspace.xml"), xml("SELECTIVE")).unwrap();
    assert!(check_ide(&root).unwrap_err().0.contains("SELECTIVE"));
    fs::write(root.join(".idea/workspace.xml"), xml("NONE")).unwrap();
    assert!(check_ide(&root).is_ok());
}

#[test]
fn checks_engram_entries_are_locked_links_into_the_package() {
    let base = scratch();
    let root = checkout();
    let package = base.join("engram/projects/p");
    fs::create_dir_all(package.join(".claude")).unwrap();
    fs::create_dir_all(package.join(".memory")).unwrap();
    fs::write(package.join("AGENTS.md"), "").unwrap();
    fs::write(base.join("claude-local.md"), "@AGENTS.local.md").unwrap();
    symlink(package.join("AGENTS.md"), root.join("AGENTS.local.md")).unwrap();
    symlink(base.join("claude-local.md"), root.join("CLAUDE.local.md")).unwrap();
    symlink(package.join(".claude"), root.join(".claude")).unwrap();
    symlink(package.join(".memory"), root.join(".memory")).unwrap();

    assert!(
        check_engram(&root, &package)
            .unwrap_err()
            .0
            .contains("isn't locked")
    );
    for name in ["AGENTS.local.md", "CLAUDE.local.md", ".claude", ".memory"] {
        lock(&root.join(name));
    }
    assert!(check_engram(&root, &package).is_ok());

    // A package entry without its link, and a link pointing elsewhere.
    fs::write(package.join(".mcp.json"), "{}").unwrap();
    assert!(
        check_engram(&root, &package)
            .unwrap_err()
            .0
            .contains(".mcp.json is missing")
    );
    symlink(base.join("claude-local.md"), root.join(".mcp.json")).unwrap();
    lock(&root.join(".mcp.json"));
    assert!(
        check_engram(&root, &package)
            .unwrap_err()
            .0
            .contains("doesn't link into")
    );
    unlock_all(&root);
}

/// Git for fixtures: none of the developer's configuration, templates or hooks. The guard
/// itself still reads the real configuration.
fn fixture_git() -> Command {
    let mut git = Command::new("git");
    git.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    git
}

fn git_checkout() -> PathBuf {
    let root = checkout();
    fs::remove_dir(root.join(".git")).unwrap();
    assert!(
        fixture_git()
            .args(["init", "-q"])
            .arg(&root)
            .status()
            .unwrap()
            .success()
    );
    root
}

fn git_config(root: &Path, key: &str, value: &str) {
    assert!(
        fixture_git()
            .arg("-C")
            .arg(root)
            .args(["config", key, value])
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn the_git_guard_reads_real_configuration() {
    let root = git_checkout();
    let home = scratch();
    let inspection = inspect(&root, &Rules::default()).unwrap();
    assert!(check_git(&root, &inspection, &home).is_ok());
    git_config(&root, "core.hooksPath", "tools/hooks");
    assert!(
        check_git(&root, &inspection, &home)
            .unwrap_err()
            .0
            .contains("tools/hooks")
    );
    git_config(&root, "core.hooksPath", ".git/hooks");
    assert!(check_git(&root, &inspection, &home).is_ok());
}

#[test]
fn nested_git_files_must_point_into_the_checkouts_git() {
    let root = git_checkout();
    let home = scratch();
    fs::create_dir_all(root.join("lib/sub")).unwrap();
    fs::write(
        root.join("lib/sub/.git"),
        "gitdir: ../../.git/modules/sub\n",
    )
    .unwrap();
    let inspection = inspect(&root, &Rules::default()).unwrap();
    assert_eq!(inspection.gitfiles, ["lib/sub/.git"]);
    assert!(check_git(&root, &inspection, &home).is_ok());
    fs::write(
        root.join("lib/sub/.git"),
        format!("gitdir: {}\n", scratch().display()),
    )
    .unwrap();
    assert!(
        check_git(&root, &inspection, &home)
            .unwrap_err()
            .0
            .contains("outside the checkout's .git")
    );
}

#[test]
fn a_projects_own_agent_configuration_is_read_only() {
    let root = std::env::temp_dir().join(format!("agent-config-{}", std::process::id()));
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join(".claude/hooks")).unwrap();
    fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink("elsewhere", root.join(".pi")).unwrap();
    let mask = vec![".codex".to_owned()];
    fs::create_dir_all(root.join(".codex")).unwrap();
    let inspection = inspect(
        &root,
        &Rules {
            mask: &mask,
            read_only: &[],
            private: &[],
        },
    )
    .unwrap();
    assert!(
        inspection.read_only.contains(&".claude".to_owned()),
        "{:?}",
        inspection.read_only
    );
    assert!(
        !inspection.read_only.contains(&".pi".to_owned()),
        "links are engram's"
    );
    assert!(
        !inspection.read_only.contains(&".codex".to_owned())
            && inspection.mask.contains(&".codex".to_owned()),
        "the policy's rule wins"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn gradle_state_is_found_in_any_spelling() {
    let root = std::env::temp_dir().join(format!("state-case-{}", std::process::id()));
    for dir in [".git", ".GRADLE", "BUILD", "app/Build", "lib/.Kotlin"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("SETTINGS.GRADLE.KTS"), "").unwrap();
    let inspection = inspect(
        &root,
        &Rules {
            mask: &[],
            read_only: &[],
            private: &[],
        },
    )
    .unwrap();
    assert_eq!(inspection.settings_roots, [""]);
    for state in [".GRADLE", "app/Build", "lib/.Kotlin"] {
        assert!(
            inspection.host_state.contains(&state.to_owned()),
            "{state}: {:?}",
            inspection.host_state
        );
    }
    assert_eq!(
        inspection.safety_net,
        ["BUILD"],
        "the root's build, as it's spelled"
    );
    assert!(
        !inspection.host_state.contains(&"BUILD".to_owned()),
        "not also masked"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_projects_own_tool_folder_gets_engrams_skills_as_locked_links() {
    let base = scratch();
    let root = checkout();
    let package = base.join("engram/projects/p");
    fs::create_dir_all(package.join(".claude/skills/engram-project")).unwrap();
    fs::write(package.join("AGENTS.md"), "").unwrap();
    fs::write(base.join("claude-local.md"), "@AGENTS.local.md").unwrap();
    symlink(package.join("AGENTS.md"), root.join("AGENTS.local.md")).unwrap();
    symlink(base.join("claude-local.md"), root.join("CLAUDE.local.md")).unwrap();
    for name in ["AGENTS.local.md", "CLAUDE.local.md"] {
        lock(&root.join(name));
    }
    // The project's own .claude, with engram's skill linked into it.
    fs::create_dir_all(root.join(".claude/skills")).unwrap();
    fs::write(root.join(".claude/settings.json"), "{}").unwrap();
    symlink(
        package.join(".claude/skills/engram-project"),
        root.join(".claude/skills/engram-project"),
    )
    .unwrap();
    assert!(
        check_engram(&root, &package)
            .unwrap_err()
            .0
            .contains("isn't locked"),
        "an unlocked skill link is refused"
    );
    lock(&root.join(".claude/skills/engram-project"));
    assert!(
        check_engram(&root, &package).is_ok(),
        "{:?}",
        check_engram(&root, &package)
    );
}

#[test]
fn git_metadata_is_read_only_whatever_the_policy_says() {
    let root = std::env::temp_dir().join(format!("git-mandatory-{}", std::process::id()));
    fs::create_dir_all(root.join(".git/hooks")).unwrap();
    let inspection = inspect(
        &root,
        &Rules {
            mask: &[],
            read_only: &[],
            private: &[],
        },
    )
    .unwrap();
    assert!(
        inspection.read_only.contains(&".git".to_owned()),
        "a minimal policy: {:?}",
        inspection.read_only
    );
    let listed = vec![".git".to_owned()];
    let inspection = inspect(
        &root,
        &Rules {
            mask: &[],
            read_only: &listed,
            private: &[],
        },
    )
    .unwrap();
    assert_eq!(
        inspection.read_only.iter().filter(|f| *f == ".git").count(),
        1,
        "listed once"
    );
    fs::remove_dir_all(root).unwrap();
}
