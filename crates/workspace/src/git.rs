//! The Git config guard: host Git must not
//! load hooks, includes or commands from files agents can write. `.git` is read-only in
//! containers, so agents can't change the configuration itself; the risk is configuration
//! (the user's, or the repository's) pointing into the writable checkout.
//!
//! Enforced for configuration files, hooks and includes; best effort for settings that run
//! commands, where only path-like words are resolved.

use crate::{Inspection, WorkspaceError, error};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Refuses the checkout if Git's configuration, as host Git reads it there, loads anything
/// from a part of the checkout agents can write. Fails closed when Git can't read it.
pub fn check_git(
    checkout: &Path,
    inspection: &Inspection,
    home: &Path,
) -> Result<(), WorkspaceError> {
    // `git config` reads configuration only: it runs no hooks, filters or fsmonitor.
    let output = Command::new("git")
        .args(["config", "--list", "--show-origin", "--show-scope", "-z"])
        .current_dir(checkout)
        .output()
        .map_err(|e| error(format!("git config: {e}")))?;
    if !output.status.success() {
        return Err(error(format!(
            "git can't read the checkout's configuration, so it can't be checked: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    check_config(
        checkout,
        &protected(inspection),
        &String::from_utf8_lossy(&output.stdout),
        home,
    )?;
    check_gitfiles(checkout, inspection)
}

/// Folders agents can't write on the host: `.git`, read-only, masked and private folders,
/// and the host's Gradle state.
fn protected(inspection: &Inspection) -> Vec<String> {
    let mut folders = vec![".git".to_owned()];
    for list in [
        &inspection.read_only,
        &inspection.mask,
        &inspection.private,
        &inspection.host_state,
        &inspection.safety_net,
    ] {
        folders.extend(list.iter().cloned());
    }
    folders
}

/// Settings whose value is a command; matched on the lowercase key.
fn runs_commands(key: &str) -> bool {
    const EXACT: &[&str] = &[
        "core.fsmonitor",
        "core.pager",
        "core.editor",
        "core.sshcommand",
        "core.askpass",
        "credential.helper",
        "gpg.program",
        "sequence.editor",
        "diff.external",
        "interactive.difffilter",
        "uploadpack.packobjectshook",
        "web.browser",
        "merge.tool",
        "diff.tool",
        "gpg.ssh.defaultkeycommand",
        "core.alternaterefscommand",
        "core.gitproxy",
        "sendemail.sendmailcmd",
        "sendemail.smtpserver",
        "sendemail.tocmd",
        "sendemail.cccmd",
    ];
    let suffix = |s: &str| key.ends_with(s);
    EXACT.contains(&key)
        || (key.starts_with("credential.") && suffix(".helper"))
        || (key.starts_with("gpg.") && suffix(".program"))
        || (key.starts_with("diff.") && (suffix(".command") || suffix(".textconv")))
        || (key.starts_with("filter.")
            && (suffix(".clean") || suffix(".smudge") || suffix(".process")))
        || (key.starts_with("merge.") && suffix(".driver"))
        || (key.starts_with("mergetool.") && (suffix(".cmd") || suffix(".path")))
        || (key.starts_with("difftool.") && (suffix(".cmd") || suffix(".path")))
        || (key.starts_with("trailer.") && (suffix(".cmd") || suffix(".command")))
        || (key.starts_with("remote.") && (suffix(".uploadpack") || suffix(".receivepack")))
        || (key.starts_with("tar.") && suffix(".command"))
        || ((key.starts_with("browser.") || key.starts_with("man."))
            && (suffix(".cmd") || suffix(".path")))
        || (key.starts_with("submodule.") && suffix(".update"))
        || key.starts_with("pager.")
        || key.starts_with("alias.")
}

/// Checks `git config --list --show-origin --show-scope -z` output.
pub fn check_config(
    checkout: &Path,
    protected: &[String],
    listing: &str,
    home: &Path,
) -> Result<(), WorkspaceError> {
    let writable = |path: &Path| -> Option<String> {
        let Some(path) = normalize(path) else {
            return Some(format!("{} (can't be resolved safely)", path.display()));
        };
        let relative = path.strip_prefix(checkout).ok()?;
        let inside = |folder: &String| relative.starts_with(folder);
        (!protected.iter().any(inside)).then(|| relative.display().to_string())
    };
    let mut records = listing.split('\0');
    while let (Some(scope), Some(origin), Some(entry)) =
        (records.next(), records.next(), records.next())
    {
        if scope.is_empty() {
            break;
        }
        let (key, value) = entry.split_once('\n').unwrap_or((entry, ""));
        let origin_file = origin
            .strip_prefix("file:")
            .map(|f| resolve(checkout, Path::new(f), home));
        if let Some(file) = &origin_file
            && let Some(relative) = writable(file)
        {
            return Err(error(format!(
                "Git reads its configuration from {relative} ({scope}), which agents can write"
            )));
        }
        let refuse = |path: &Path, what: &str| match writable(path) {
            Some(relative) => Err(error(format!(
                "Git's {key} = {value:?} ({scope}) {what} {relative}, which agents can write; host Git would run what they put there"
            ))),
            None => Ok(()),
        };
        if key == "core.hookspath" {
            refuse(&resolve(checkout, Path::new(value), home), "points to")?;
        } else if key == "include.path" || (key.starts_with("includeif.") && key.ends_with(".path"))
        {
            // Relative to the file that includes it; refused even if it doesn't exist yet.
            let base = origin_file
                .as_deref()
                .and_then(Path::parent)
                .unwrap_or(checkout);
            refuse(&resolve(base, Path::new(value), home), "includes")?;
        } else if runs_commands(key) {
            // Words between spaces and shell delimiters (;, &&, ||, |, $(…), backticks,
            // redirections) to expose path-like words.
            let words = value
                .split(|c: char| c.is_whitespace() || ";&|()<>`$".contains(c))
                .filter(|w| !w.is_empty());
            for word in words.map(|w| w.trim_matches(|c| c == '"' || c == '\'' || c == '!')) {
                if word.contains('/') {
                    refuse(&resolve(checkout, Path::new(word), home), "runs")?;
                }
            }
        }
    }
    Ok(())
}

/// Nested `.git` files (submodules, worktrees) must point into the checkout's own `.git`:
/// elsewhere, host Git would read configuration and hooks from wherever they point.
fn check_gitfiles(checkout: &Path, inspection: &Inspection) -> Result<(), WorkspaceError> {
    for file in &inspection.gitfiles {
        let path = checkout.join(file);
        let text = fs::read_to_string(&path).map_err(|e| error(format!("{file}: {e}")))?;
        let target = text
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .ok_or_else(|| error(format!("{file} is not a gitdir file")))?;
        let target = normalize(&path.parent().unwrap_or(checkout).join(target)).unwrap_or_default();
        if !target.starts_with(checkout.join(".git")) {
            return Err(error(format!(
                "{file} points Git to {}, outside the checkout's .git",
                target.display()
            )));
        }
    }
    Ok(())
}

/// `~/` expands to `home`; relative paths are relative to `base`.
fn resolve(base: &Path, path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => base.join(path),
    }
}

/// Resolves the path as the file system would: each existing link before the `..` after it.
/// None when that can't be known: a `..` after a missing folder (an agent could create it as a
/// link anywhere), or a link loop.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut pending: Vec<PathBuf> = path
        .components()
        .rev()
        .map(|c| PathBuf::from(c.as_os_str()))
        .collect();
    let mut existing = PathBuf::new();
    let mut missing = PathBuf::new();
    let mut links = 0;
    while let Some(next) = pending.pop() {
        match next.components().next() {
            Some(Component::CurDir) => {}
            Some(Component::ParentDir) if missing.as_os_str().is_empty() => {
                existing.pop();
            }
            Some(Component::ParentDir) => return None,
            Some(Component::RootDir | Component::Prefix(_)) => existing.push(&next),
            Some(Component::Normal(name)) if missing.as_os_str().is_empty() => {
                let candidate = existing.join(name);
                match fs::symlink_metadata(&candidate) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        links += 1;
                        if links > 40 {
                            return None;
                        }
                        let target = fs::read_link(&candidate).ok()?;
                        if target.is_absolute() {
                            existing = PathBuf::new();
                        }
                        pending.extend(
                            target
                                .components()
                                .rev()
                                .map(|c| PathBuf::from(c.as_os_str())),
                        );
                    }
                    Ok(_) => existing = candidate,
                    Err(_) => missing.push(name),
                }
            }
            Some(Component::Normal(name)) => missing.push(name),
            None => {}
        }
    }
    // Canonical spelling of what exists (case-insensitive file systems), then the rest.
    let mut resolved = fs::canonicalize(&existing).unwrap_or(existing);
    resolved.push(missing);
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(scope: &str, origin: &str, key: &str, value: &str) -> String {
        format!("{scope}\0{origin}\0{key}\n{value}\0")
    }

    fn check(listing: &str) -> Result<(), WorkspaceError> {
        let checkout = std::env::temp_dir().join(format!("git-guard-test-{}", std::process::id()));
        fs::create_dir_all(checkout.join(".git")).unwrap();
        let checkout = fs::canonicalize(checkout).unwrap();
        let listing = listing.replace("CHECKOUT", &checkout.display().to_string());
        check_config(
            &checkout,
            &[".git".into(), "gradle/wrapper".into()],
            &listing,
            Path::new("/Users/me"),
        )
    }

    #[test]
    fn allows_ordinary_configuration() {
        let listing = [
            record(
                "global",
                "file:/Users/me/.gitconfig",
                "core.hookspath",
                "~/.githooks",
            ),
            record(
                "global",
                "file:/Users/me/.gitconfig",
                "credential.helper",
                "osxkeychain",
            ),
            record("local", "file:.git/config", "core.hookspath", ".git/hooks"),
            record("local", "file:.git/config", "include.path", "local.cfg"),
            record(
                "local",
                "file:.git/config",
                "filter.lfs.clean",
                "git-lfs clean -- %f",
            ),
            record("local", "file:.git/config", "alias.st", "status"),
        ]
        .concat();
        assert!(check(&listing).is_ok(), "{:?}", check(&listing));
    }

    #[test]
    fn refuses_hooks_includes_and_config_files_in_the_writable_checkout() {
        for (listing, expected) in [
            (
                record("local", "file:.git/config", "core.hookspath", "hooks"),
                "core.hookspath",
            ),
            (
                record(
                    "local",
                    "file:.git/config",
                    "core.hookspath",
                    "./.git/../tools/hooks",
                ),
                "tools/hooks",
            ),
            (
                record(
                    "global",
                    "file:/Users/me/.gitconfig",
                    "core.hookspath",
                    "CHECKOUT/scripts/hooks",
                ),
                "scripts/hooks",
            ),
            (
                record(
                    "local",
                    "file:.git/config",
                    "include.path",
                    "../shared.gitconfig",
                ),
                "includes shared.gitconfig",
            ),
            (
                record(
                    "local",
                    "file:.git/config",
                    "includeif.onbranch:main.path",
                    "../x.cfg",
                ),
                "includes x.cfg",
            ),
            (
                record("local", "file:.git/../extra.cfg", "core.pager", "less"),
                "reads its configuration from extra.cfg",
            ),
            (
                record(
                    "local",
                    "file:.git/config",
                    "core.fsmonitor",
                    "./tools/watch",
                ),
                "runs tools/watch",
            ),
            (
                record("local", "file:.git/config", "alias.x", "!sh scripts/run.sh"),
                "runs scripts/run.sh",
            ),
            (
                record(
                    "local",
                    "file:.git/config",
                    "filter.x.smudge",
                    "app/bin/smudge %f",
                ),
                "runs app/bin/smudge",
            ),
            (
                record(
                    "local",
                    "file:.git/config",
                    "core.fsmonitor",
                    "/usr/bin/true;./scripts/watch",
                ),
                "runs scripts/watch",
            ),
            (
                record(
                    "local",
                    "file:.git/config",
                    "core.pager",
                    "less && tools/run",
                ),
                "runs tools/run",
            ),
            (
                record(
                    "local",
                    "file:.git/config",
                    "alias.y",
                    "!git log|app/bin/filter",
                ),
                "runs app/bin/filter",
            ),
            (
                record(
                    "local",
                    "file:.git/config",
                    "core.editor",
                    "vi $(./scripts/pick)",
                ),
                "runs scripts/pick",
            ),
        ] {
            let e = check(&listing).expect_err(&listing).0;
            assert!(e.contains(expected), "{listing:?}: {e}");
        }
    }

    #[test]
    fn refuses_every_command_family_running_a_checkout_script() {
        for key in [
            "gpg.ssh.defaultkeycommand",
            "trailer.sign.cmd",
            "trailer.sign.command",
            "core.alternaterefscommand",
            "remote.origin.uploadpack",
            "remote.origin.receivepack",
            "tar.tgz.command",
            "browser.x.cmd",
            "browser.x.path",
            "man.x.cmd",
            "man.x.path",
            "core.gitproxy",
            "submodule.lib.update",
            "sendemail.sendmailcmd",
            "sendemail.smtpserver",
            "sendemail.tocmd",
            "sendemail.cccmd",
        ] {
            let listing = record("local", "file:.git/config", key, "!./scripts/canary");
            let e = check(&listing).expect_err(key).0;
            assert!(e.contains("runs scripts/canary"), "{key}: {e}");
        }
    }

    #[test]
    fn follows_links_before_parent_folders() {
        // The checkout `check` uses.
        let checkout = std::env::temp_dir().join(format!("git-guard-test-{}", std::process::id()));
        fs::create_dir_all(checkout.join("scripts/deep")).unwrap();
        let _ = std::os::unix::fs::symlink("../scripts/deep", checkout.join(".git/link"));
        let _ =
            std::os::unix::fs::symlink("../scripts/nowhere/deep", checkout.join(".git/dangling"));
        for (value, expected) in [
            (".git/link/../hooks", "scripts/hooks"),
            (".git/dangling/../hooks", "can't be resolved"),
        ] {
            let e = check(&record(
                "local",
                "file:.git/config",
                "core.hookspath",
                value,
            ))
            .expect_err(value)
            .0;
            assert!(e.contains(expected), "{value}: {e}");
        }
    }

    #[test]
    fn allows_paths_into_protected_folders() {
        let listing = [
            record(
                "local",
                "file:.git/config",
                "core.hookspath",
                "gradle/wrapper/hooks",
            ),
            record("local", "file:.git/config", "include.path", "extra.cfg"),
        ]
        .concat();
        assert!(check(&listing).is_ok());
    }
}
