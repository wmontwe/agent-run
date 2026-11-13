//! Checks a container's mounts against the plan before the gate releases anything
//! project-controlled. Each mount type has its own identity; no single field is
//! trusted for all of them.

use crate::report::Report;
use crate::{Kind, Plan};
use std::collections::BTreeSet;

/// Problems found; empty when the container matches the plan. `report.targets` must be
/// indexed like `plan.mounts`.
pub fn verify(plan: &Plan, report: &Report) -> Vec<String> {
    let mut problems = Vec::new();
    let checkout = plan.checkout.display().to_string();
    let Some(workspace) = report.visible(&checkout) else {
        return vec!["checkout not mounted".to_owned()];
    };
    if report.targets.len() != plan.mounts.len() {
        return vec![format!(
            "report has {} targets, plan has {}",
            report.targets.len(),
            plan.mounts.len()
        )];
    }

    let planned: BTreeSet<String> = plan
        .mounts
        .iter()
        .map(|m| plan.target(m).display().to_string())
        .collect();
    for mount in &report.mounts {
        let inside = mount
            .mount_point
            .strip_prefix(&checkout)
            .is_some_and(|rest| rest.starts_with('/'));
        if inside && !planned.contains(&mount.mount_point) {
            problems.push(format!(
                "unplanned mount {}",
                relative(&checkout, &mount.mount_point)
            ));
        }
    }

    for (index, item) in plan.mounts.iter().enumerate() {
        let target = plan.target(item).display().to_string();
        let name = if item.relative.is_empty() {
            "checkout"
        } else {
            item.relative.as_str()
        };
        let Some(mount) = report.visible(&target) else {
            problems.push(format!("{name} not mounted"));
            continue;
        };
        match &item.kind {
            Kind::Workspace | Kind::Pin | Kind::ReadOnly => {
                // Same file share, the planned folder within it, the planned options, and
                // the host's inode: the path alone cannot detect a rearranged folder.
                let expected_root = format!("{}{}", workspace.root, &target[checkout.len()..]);
                if mount.fs_type != workspace.fs_type || mount.root != expected_root {
                    problems.push(format!("{name}: source {} {}", mount.fs_type, mount.root));
                }
                if mount.is_read_only() != (item.kind == Kind::ReadOnly) {
                    problems.push(format!(
                        "{name}: mounted {}",
                        if mount.is_read_only() {
                            "read-only"
                        } else {
                            "read-write"
                        }
                    ));
                }
                if report.targets[index].map(|t| t.ino) != item.host_ino {
                    problems.push(format!("{name}: not the folder planned (inode differs)"));
                }
            }
            Kind::Mask => {
                if mount.fs_type != "tmpfs" || !mount.is_read_only() {
                    problems.push(format!(
                        "{name}: not masked ({} {})",
                        mount.fs_type,
                        mount.options.join(",")
                    ));
                }
            }
            Kind::Hide => {
                if mount.root != "/null" || !mount.is_read_only() {
                    problems.push(format!(
                        "{name}: not hidden ({} {})",
                        mount.fs_type, mount.root
                    ));
                }
            }
            Kind::Private { subpath } => {
                let expected = format!("/volumes/{}/_data/{subpath}", plan.volume);
                if !mount.root.ends_with(&expected) || mount.fs_type == workspace.fs_type {
                    problems.push(format!("{name}: source {} {}", mount.fs_type, mount.root));
                }
            }
        }
    }
    problems
}

fn relative(checkout: &str, path: &str) -> String {
    path.strip_prefix(checkout)
        .unwrap_or(path)
        .trim_start_matches('/')
        .to_owned()
}

/// Where the gate is in every image agent-run builds.
pub const GATE: &str = "/usr/local/bin/agent-run-gate";

/// Checks a gated container's mounts against the plan (the gate reports what it sees), then
/// releases it to run its command. Nothing runs if they don't match.
pub fn verify_and_release(
    podman: &podman::Podman,
    container: &str,
    plan: &crate::Plan,
) -> Result<(), String> {
    check_mounts(podman, container, plan)?;
    release(podman, container)
}

/// Releases a verified gated container: its command starts.
pub fn release(podman: &podman::Podman, container: &str) -> Result<(), String> {
    podman
        .run(&["exec", container, GATE, "release"])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Checks a gated container's mounts against the plan, without releasing it.
pub fn check_mounts(
    podman: &podman::Podman,
    container: &str,
    plan: &crate::Plan,
) -> Result<(), String> {
    let mut args = vec![
        "exec".to_owned(),
        container.to_owned(),
        GATE.into(),
        "report".into(),
    ];
    args.extend(
        plan.mounts
            .iter()
            .map(|m| plan.target(m).display().to_string()),
    );
    let report = podman.run(&args).map_err(|e| e.to_string())?;
    let report = crate::parse_report(&report, plan.mounts.len())?;
    match verify(plan, &report).as_slice() {
        [] => Ok(()),
        problems => Err(format!(
            "the container doesn't match the mount plan, so nothing was started: {}",
            problems.join("; ")
        )),
    }
}
