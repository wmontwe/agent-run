//! Inspects a checkout before mount planning: which folders the rules name, where the host's Gradle state is, and
//! whether anything on the host would act on agent edits by itself.
//!
//! Nothing here changes the checkout. Paths in results are relative to the checkout
//! root, joined with `/`; the root itself is the empty string.

mod android;
mod git;
mod location;

pub use android::sdk_dir;
pub use git::{check_config, check_git};
pub use location::check_location;

use std::fmt;
use std::fs;
use std::os::macos::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The rule folders from the policy, as exact relative paths.
#[derive(Debug, Clone, Default)]
pub struct Rules<'a> {
    pub mask: &'a [String],
    pub read_only: &'a [String],
    pub private: &'a [String],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inspection {
    /// Folders holding a settings script: the root build and each included build.
    pub settings_roots: Vec<String>,
    /// The host's Gradle state to mask: every `build`, `.gradle` and `.kotlin` folder,
    /// except the `build` of a settings root (see `safety_net`).
    pub host_state: Vec<String>,
    /// The `build` folder of each settings root, mounted private whether it exists or
    /// not, so a build failing before its redirection applies can't write the checkout.
    pub safety_net: Vec<String>,
    /// Rule folders that exist, by kind.
    pub mask: Vec<String>,
    pub read_only: Vec<String>,
    pub private: Vec<String>,
    /// Rule folders that don't exist; nothing is mounted or created for them.
    pub absent: Vec<String>,
    /// `.git` files below the root (submodules, worktrees), checked by [`check_git`].
    pub gitfiles: Vec<String>,
    /// Single files hidden from containers: `local.properties`. A mask over a file is bypassed
    /// through another spelling on a case-insensitive checkout, so it may only hold what isn't
    /// secret (see [`android`]).
    pub hidden: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceError(pub String);

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WorkspaceError {}

fn error(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError(message.into())
}

const STATE_NAMES: [&str; 3] = ["build", ".gradle", ".kotlin"];
const SETTINGS_SCRIPTS: [&str; 2] = ["settings.gradle.kts", "settings.gradle"];

/// Inspects `checkout`, which must be canonical.
/// Agent tools' project configuration (hooks, MCP servers, skills): host agents act on it, so a
/// real folder of these at the root is read-only in containers, whatever the policy lists.
/// Links are engram's, checked by [`check_engram`].
pub const AGENT_CONFIG: [&str; 8] = [
    ".agents", ".air", ".claude", ".codex", ".cursor", ".gemini", ".junie", ".pi",
];

pub fn inspect(checkout: &Path, rules: &Rules) -> Result<Inspection, WorkspaceError> {
    let git = fs::symlink_metadata(checkout.join(".git"))
        .map_err(|_| error("the checkout has no .git folder"))?;
    if !git.is_dir() {
        return Err(error(
            ".git is not a folder (worktrees and submodule checkouts aren't supported)",
        ));
    }

    let mut inspection = Inspection::default();
    if android::check_local_properties(checkout)? {
        inspection.hidden.push("local.properties".to_owned());
    }
    for (kind, folders) in [
        ("mask", rules.mask),
        ("read-only", rules.read_only),
        ("private", rules.private),
    ] {
        for folder in folders {
            match rule_folder(checkout, folder)
                .map_err(|e| error(format!("workspace.{kind} \"{folder}\": {e}")))?
            {
                false => inspection.absent.push(folder.clone()),
                true => match kind {
                    "mask" => inspection.mask.push(folder.clone()),
                    "read-only" => inspection.read_only.push(folder.clone()),
                    _ => inspection.private.push(folder.clone()),
                },
            }
        }
    }
    // Git's metadata holds hooks and configuration host Git runs: read-only whatever the
    // policy lists, as the Git guard assumes (check_git).
    if ![&inspection.mask, &inspection.read_only, &inspection.private]
        .iter()
        .any(|l| l.iter().any(|f| f == ".git"))
    {
        inspection.read_only.push(".git".to_owned());
    }
    for name in AGENT_CONFIG {
        let is_real_folder = fs::symlink_metadata(checkout.join(name)).is_ok_and(|m| m.is_dir());
        let listed = [&inspection.mask, &inspection.read_only, &inspection.private]
            .iter()
            .any(|l| l.iter().any(|f| f == name));
        if is_real_folder && !listed {
            inspection.read_only.push(name.to_owned());
        }
    }

    let skip: Vec<&str> = rules
        .mask
        .iter()
        .chain(rules.read_only)
        .chain(rules.private)
        .map(String::as_str)
        .collect();
    let mut state = Vec::new();
    walk(
        checkout,
        "",
        &skip,
        &mut state,
        &mut inspection.settings_roots,
        &mut inspection.gitfiles,
    )?;
    inspection.settings_roots.sort();
    for root in &inspection.settings_roots {
        // The root's build folder as it's spelled, if it exists: one folder, one mount.
        let spelled = state
            .iter()
            .find(|f| f.eq_ignore_ascii_case(&join(root, "build")))
            .cloned();
        inspection
            .safety_net
            .push(spelled.unwrap_or_else(|| join(root, "build")));
    }
    state.retain(|folder| !inspection.safety_net.contains(folder));
    state.sort();
    inspection.host_state = state;
    Ok(inspection)
}

/// Whether a rule's folder exists. It must be a real folder, with no symlink anywhere
/// in its path: a mount would land where a link points.
fn rule_folder(checkout: &Path, folder: &str) -> Result<bool, String> {
    let mut path = checkout.to_path_buf();
    for segment in folder.split('/') {
        path.push(segment);
        match fs::symlink_metadata(&path) {
            Err(_) => return Ok(false),
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("{} is a symlink", relative(checkout, &path)));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(format!("{} is not a folder", relative(checkout, &path)));
            }
            Ok(_) => {}
        }
    }
    Ok(true)
}

/// Collects state folders and settings roots below `relative_dir`, without following
/// symlinks and without entering `.git`, `node_modules`, rule folders or state folders.
fn walk(
    checkout: &Path,
    relative_dir: &str,
    skip: &[&str],
    state: &mut Vec<String>,
    settings_roots: &mut Vec<String>,
    gitfiles: &mut Vec<String>,
) -> Result<(), WorkspaceError> {
    let dir = checkout.join(relative_dir);
    let entries = fs::read_dir(&dir).map_err(|e| error(format!("{}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| error(format!("{}: {e}", dir.display())))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = join(relative_dir, &name);
        let kind = entry
            .file_type()
            .map_err(|e| error(format!("{child}: {e}")))?;
        // Any spelling: on a case-insensitive checkout, host Gradle's `build` is a `BUILD` too.
        let lower = name.to_lowercase();
        if STATE_NAMES.contains(&lower.as_str()) {
            if kind.is_symlink() || !kind.is_dir() {
                return Err(error(format!(
                    "{child} must be a folder or absent: Gradle state can't be protected through a link or file"
                )));
            }
            state.push(child);
        } else if name == ".git" && kind.is_file() && !relative_dir.is_empty() {
            gitfiles.push(child);
        } else if SETTINGS_SCRIPTS.contains(&lower.as_str()) && kind.is_file() {
            if !settings_roots.contains(&relative_dir.to_owned()) {
                settings_roots.push(relative_dir.to_owned());
            }
        } else if kind.is_dir()
            && !matches!(name.as_str(), ".git" | "node_modules")
            && !skip.contains(&child.as_str())
        {
            walk(checkout, &child, skip, state, settings_roots, gitfiles)?;
        }
    }
    Ok(())
}

/// Refuses a checkout whose IntelliJ project reloads Gradle on external changes: agent
/// edits to build scripts would then run on the host through a sync. Without `.idea/`
/// there's nothing to check.
pub fn check_ide(checkout: &Path) -> Result<(), WorkspaceError> {
    if !checkout.join(".idea").is_dir() {
        return Ok(());
    }
    let workspace = fs::read_to_string(checkout.join(".idea/workspace.xml")).unwrap_or_default();
    let setting = workspace
        .split("<component name=\"AutoImportSettings\">")
        .nth(1)
        .and_then(|component| component.split("</component>").next())
        .and_then(|component| component.split("name=\"autoReloadType\" value=\"").nth(1))
        .and_then(|rest| rest.split('"').next());
    match setting {
        Some("NONE") => Ok(()),
        other => Err(error(format!(
            "IntelliJ reloads the Gradle project on external changes ({}): agent edits to build scripts would run on the \
             host. Set Settings > Build Tools > \"Reload project after changes in the build scripts\" to off.",
            other.unwrap_or("default")
        ))),
    }
}

/// Engram's entries in the checkout root.
pub const ENGRAM_LINKED: [&str; 8] = [
    ".agents",
    ".air",
    ".claude",
    ".codex",
    ".junie",
    ".mcp.json",
    ".memory",
    ".pi",
];
pub const ENGRAM_ENTRY_POINTS: [&str; 2] = ["AGENTS.local.md", "CLAUDE.local.md"];

/// Refuses an engram project whose entries aren't all locked links: an entry the package
/// provides, and both entry points, must be symlinks with the immutable flag, so no
/// container can replace one. `package` is the project's engram package.
pub fn check_engram(checkout: &Path, package: &Path) -> Result<(), WorkspaceError> {
    let package = fs::canonicalize(package)
        .map_err(|e| error(format!("engram package {}: {e}", package.display())))?;
    let expected = ENGRAM_ENTRY_POINTS.into_iter().chain(
        ENGRAM_LINKED
            .into_iter()
            .filter(|name| fs::symlink_metadata(package.join(name)).is_ok()),
    );
    for name in expected {
        let path = checkout.join(name);
        let meta = fs::symlink_metadata(&path)
            .map_err(|_| error(format!("engram entry {name} is missing; run engram stow")))?;
        // The project's own tool folder: engram links only its skills into it (the folder is
        // read-only in containers as a project's agent configuration).
        if meta.is_dir() && AGENT_CONFIG.contains(&name) {
            check_engram_skills(&path, &package.join(name).join("skills"))?;
            continue;
        }
        if !meta.file_type().is_symlink() {
            return Err(error(format!(
                "{name} is not engram's link; run engram stow"
            )));
        }
        if meta.st_flags() & UF_IMMUTABLE == 0 {
            return Err(error(format!(
                "engram link {name} isn't locked; run engram stow"
            )));
        }
        if name != "CLAUDE.local.md" {
            let target = if name == "AGENTS.local.md" {
                package.join("AGENTS.md")
            } else {
                package.join(name)
            };
            if fs::canonicalize(&path).ok() != fs::canonicalize(&target).ok() {
                return Err(error(format!(
                    "{name} doesn't link into the engram package {}",
                    package.display()
                )));
            }
        }
    }
    Ok(())
}

/// In a project's own tool folder, engram's skill links (those leading into the package's
/// skills) must be locked like engram's other links.
fn check_engram_skills(folder: &Path, package_skills: &Path) -> Result<(), WorkspaceError> {
    let Ok(package_skills) = fs::canonicalize(package_skills) else {
        return Ok(());
    };
    for entry in fs::read_dir(folder.join("skills"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let into_package =
            fs::canonicalize(entry.path()).is_ok_and(|t| t.starts_with(&package_skills));
        if meta.file_type().is_symlink() && into_package && meta.st_flags() & UF_IMMUTABLE == 0 {
            let name = entry.path().display().to_string();
            return Err(error(format!(
                "engram's skill link {name} isn't locked; run engram stow"
            )));
        }
    }
    Ok(())
}

const UF_IMMUTABLE: u32 = 0x2;

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn relative(checkout: &Path, path: &Path) -> String {
    path.strip_prefix(checkout)
        .map(PathBuf::from)
        .unwrap_or_default()
        .display()
        .to_string()
}

#[cfg(test)]
mod tests;
