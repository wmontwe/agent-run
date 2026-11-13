//! The mount plan of a project pod: the checkout at its host
//! path, every rule folder and the host's Gradle state, with pinned parents, turned into
//! Podman arguments, and the check of a container's report against it.
//!
//! Every container in a pod uses the same plan. It's stored in text form when the pod
//! starts, so warm starts verify against exactly the plan the pod began with.

mod report;
mod verify;

pub use report::{MountInfo, Report, TargetStat, parse_report};
pub use verify::{GATE, check_mounts, release, verify, verify_and_release};

use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use workspace::Inspection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// The checkout itself, read-write.
    Workspace,
    /// A parent of a protected folder, bind-mounted onto itself, read-write, so it can't be
    /// renamed or removed (which would move the protection along with it).
    Pin,
    /// A folder agents read but can't change.
    ReadOnly,
    /// A folder hidden by `--security-opt mask`: an empty read-only tmpfs.
    Mask,
    /// A single file hidden by `--security-opt mask`: `/dev/null`, read-only. Bypassed through
    /// another spelling on a case-insensitive checkout, so only for files without secrets.
    Hide,
    /// A folder replaced by a subpath of the project's state volume.
    Private { subpath: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    /// Path relative to the checkout; empty for the checkout itself.
    pub relative: String,
    pub kind: Kind,
    /// The host's inode for bind-mounted folders: what tells a rearranged folder apart.
    pub host_ino: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub checkout: PathBuf,
    /// The project's state volume, for `Private` mounts.
    pub volume: String,
    /// The checkout first, then by depth: parents before what's mounted inside them.
    pub mounts: Vec<Mount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError(pub String);

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlanError {}

fn error(message: impl Into<String>) -> PlanError {
    PlanError(message.into())
}

/// Builds the plan for a canonical `checkout` from its inspection.
pub fn plan(checkout: &Path, inspection: &Inspection, volume: &str) -> Result<Plan, PlanError> {
    check_chars(&checkout.display().to_string())?;
    let mut targets: Vec<(String, Kind)> = Vec::new();
    targets.extend(
        inspection
            .read_only
            .iter()
            .map(|r| (r.clone(), Kind::ReadOnly)),
    );
    targets.extend(
        inspection
            .mask
            .iter()
            .chain(&inspection.host_state)
            .map(|r| (r.clone(), Kind::Mask)),
    );
    targets.extend(inspection.hidden.iter().map(|r| (r.clone(), Kind::Hide)));
    targets.extend(
        inspection
            .private
            .iter()
            .chain(&inspection.safety_net)
            .map(|r| {
                (
                    r.clone(),
                    Kind::Private {
                        subpath: format!("private/{r}"),
                    },
                )
            }),
    );

    let named: BTreeSet<&str> = targets.iter().map(|(r, _)| r.as_str()).collect();
    if named.len() != targets.len() {
        return Err(error("a folder appears under two rules"));
    }
    let mut pins = BTreeSet::new();
    for (relative, _) in &targets {
        for parent in Path::new(relative)
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            let parent = parent.display().to_string();
            if !named.contains(parent.as_str()) {
                pins.insert(parent);
            }
        }
    }

    let mut mounts = vec![host_mount(checkout, "", Kind::Workspace)?];
    let mut rest = Vec::new();
    for pin in pins {
        rest.push(host_mount(checkout, &pin, Kind::Pin)?);
    }
    for (relative, kind) in targets {
        check_chars(&relative)?;
        check_no_symlinks(checkout, &relative)?;
        rest.push(match kind {
            Kind::ReadOnly => host_mount(checkout, &relative, Kind::ReadOnly)?,
            kind => Mount {
                relative,
                kind,
                host_ino: None,
            },
        });
    }
    rest.sort_by(|a, b| {
        depth(&a.relative)
            .cmp(&depth(&b.relative))
            .then_with(|| a.relative.cmp(&b.relative))
    });
    mounts.extend(rest);
    Ok(Plan {
        checkout: checkout.to_path_buf(),
        volume: volume.to_owned(),
        mounts,
    })
}

fn host_mount(checkout: &Path, relative: &str, kind: Kind) -> Result<Mount, PlanError> {
    check_chars(relative)?;
    check_no_symlinks(checkout, relative)?;
    let path = checkout.join(relative);
    let meta =
        fs::symlink_metadata(&path).map_err(|e| error(format!("{}: {e}", path.display())))?;
    if kind == Kind::Hide {
        if !meta.is_file() {
            return Err(error(format!("{} is not a regular file", path.display())));
        }
    } else if !meta.is_dir() {
        return Err(error(format!("{} is not a folder", path.display())));
    }
    Ok(Mount {
        relative: relative.to_owned(),
        kind,
        host_ino: Some(meta.ino()),
    })
}

/// No component of a mount target's path may be a symlink: a mount would land where it
/// points. Components that don't exist yet are fine (Podman creates mount points).
fn check_no_symlinks(checkout: &Path, relative: &str) -> Result<(), PlanError> {
    let mut path = checkout.to_path_buf();
    for segment in relative.split('/').filter(|s| !s.is_empty()) {
        path.push(segment);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(error(format!(
                "{} is a symlink; a mount can't protect it",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Podman's `--mount` splits on commas and `mask=` on colons; the text form on tabs and
/// newlines. Paths with them are refused rather than escaped.
fn check_chars(path: &str) -> Result<(), PlanError> {
    match path
        .chars()
        .find(|c| matches!(c, ',' | ':' | '\t' | '\n' | '\r'))
    {
        Some(c) => Err(error(format!(
            "{path:?} contains {c:?}, which agent-run can't pass to Podman safely"
        ))),
        None => Ok(()),
    }
}

fn depth(relative: &str) -> usize {
    relative.split('/').filter(|s| !s.is_empty()).count()
}

impl Plan {
    /// The container path of a mount: the same as on the host.
    pub fn target(&self, mount: &Mount) -> PathBuf {
        if mount.relative.is_empty() {
            self.checkout.clone()
        } else {
            self.checkout.join(&mount.relative)
        }
    }

    /// Podman arguments for the plan's mounts.
    pub fn podman_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        let mut masks = Vec::new();
        for mount in &self.mounts {
            let target = self.target(mount).display().to_string();
            match &mount.kind {
                Kind::Workspace | Kind::Pin => args.extend([
                    "--mount".to_owned(),
                    format!("type=bind,src={target},dst={target}"),
                ]),
                Kind::ReadOnly => args.extend([
                    "--mount".to_owned(),
                    format!("type=bind,src={target},dst={target},ro"),
                ]),
                Kind::Mask | Kind::Hide => masks.push(target),
                Kind::Private { subpath } => args.extend([
                    "--mount".to_owned(),
                    // Shared label, relabeled at each mount: a file a container moved in from its
                    // own tmpfs keeps that container's SELinux categories, which the next pod's
                    // containers can't read.
                    format!(
                        "type=volume,src={},dst={target},subpath={subpath},relabel=shared",
                        self.volume
                    ),
                ]),
            }
        }
        if !masks.is_empty() {
            args.extend([
                "--security-opt".to_owned(),
                format!("mask={}", masks.join(":")),
            ]);
        }
        args
    }

    /// Volume subpaths that must exist before a container starts: Podman mounts a
    /// subpath only if it's there.
    pub fn subpaths(&self) -> Vec<&str> {
        self.mounts
            .iter()
            .filter_map(|m| match &m.kind {
                Kind::Private { subpath } => Some(subpath.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The plan as text, one mount per line, for storing with the pod.
    pub fn to_text(&self) -> String {
        let mut text = format!(
            "checkout\t{}\nvolume\t{}\n",
            self.checkout.display(),
            self.volume
        );
        for mount in &self.mounts {
            let ino = mount
                .host_ino
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".to_owned());
            let (kind, extra) = match &mount.kind {
                Kind::Workspace => ("workspace", ""),
                Kind::Pin => ("pin", ""),
                Kind::ReadOnly => ("read-only", ""),
                Kind::Mask => ("mask", ""),
                Kind::Hide => ("hide", ""),
                Kind::Private { subpath } => ("private", subpath.as_str()),
            };
            text.push_str(&format!("{kind}\t{}\t{ino}\t{extra}\n", mount.relative));
        }
        text
    }

    pub fn from_text(text: &str) -> Result<Plan, PlanError> {
        let bad = |line: &str| error(format!("bad plan line: {line:?}"));
        let mut lines = text.lines();
        let checkout = lines
            .next()
            .and_then(|l| l.strip_prefix("checkout\t"))
            .ok_or_else(|| bad("checkout"))?;
        let volume = lines
            .next()
            .and_then(|l| l.strip_prefix("volume\t"))
            .ok_or_else(|| bad("volume"))?;
        let mut mounts = Vec::new();
        for line in lines {
            let [kind, relative, ino, extra] = line.split('\t').collect::<Vec<_>>()[..] else {
                return Err(bad(line));
            };
            let kind = match kind {
                "workspace" => Kind::Workspace,
                "pin" => Kind::Pin,
                "read-only" => Kind::ReadOnly,
                "mask" => Kind::Mask,
                "hide" => Kind::Hide,
                "private" => Kind::Private {
                    subpath: extra.to_owned(),
                },
                _ => return Err(bad(line)),
            };
            let host_ino = if ino == "-" {
                None
            } else {
                Some(ino.parse().map_err(|_| bad(line))?)
            };
            mounts.push(Mount {
                relative: relative.to_owned(),
                kind,
                host_ino,
            });
        }
        Ok(Plan {
            checkout: PathBuf::from(checkout),
            volume: volume.to_owned(),
            mounts,
        })
    }

    /// A short identity of the plan, recorded on the pod.
    pub fn hash(&self) -> String {
        hex(&Sha256::digest(self.to_text()))
    }
}

/// A short identity of the policy's workspace rules: a pod refuses new sessions once
/// they differ from what it started with.
pub fn rules_hash(mask: &[String], read_only: &[String], private: &[String]) -> String {
    let text = format!(
        "mask\t{}\nread-only\t{}\nprivate\t{}\n",
        mask.join("\t"),
        read_only.join("\t"),
        private.join("\t")
    );
    hex(&Sha256::digest(text))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
