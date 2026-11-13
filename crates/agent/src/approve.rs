//! `agent allow`, `revoke`, `denied` and `policy show`, about the project containing the current folder. Approvals are
//! always one agent's. The decisions are the approvals crate's; this finds the project and the
//! agent and applies the result.

use crate::services::{Services, session_identities};
use crate::{Paths, home_dir};
use approvals::Scope;
use pod::Project;
use podman::{Object, Podman};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn project(podman: &Podman, paths: &Paths, folder: &Path) -> Result<Project, String> {
    let checkout =
        pod::checkout_for(podman, &paths.namespace, folder).map_err(|e| e.to_string())?;
    Ok(Project::new(&paths.namespace, &checkout))
}

/// The agent an approval is for: the named one, else the one running in the project.
fn agent_for(podman: &Podman, project: &Project, named: Option<&str>) -> Result<String, String> {
    if let Some(name) = named {
        return agents::Agent::parse(name)
            .map(|a| a.name().to_owned())
            .ok_or_else(|| format!("unknown agent {name}"));
    }
    let mut running: Vec<String> = if podman
        .exists(Object::Pod, &project.pod())
        .map_err(|e| e.to_string())?
    {
        session_identities(podman, project)?
            .into_iter()
            .map(|(_, agent)| agent)
            .collect()
    } else {
        Vec::new()
    };
    running.sort();
    running.dedup();
    match running.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "no agent runs in {}; name the agent with --agent",
            project.checkout.display()
        )),
        several => Err(format!(
            "{} run in {}; name the agent with --agent",
            several.join(" and "),
            project.checkout.display()
        )),
    }
}

/// Applies an approval change, for one agent unless `agent` is None (the shared list), then
/// the proxy's rules if the proxy runs. The rules are applied even when the change itself is
/// refused: an earlier change whose reload failed is still pending, and repeating the command
/// is how the user retries it. Messages are printed only once the proxy has the change.
fn change(
    paths: &Paths,
    folder: &Path,
    agent: Option<Option<&str>>,
    apply: impl FnOnce(&approvals::Project, &policy::Policy, bool) -> Result<Vec<String>, String>,
) -> Result<(), String> {
    let podman = Podman::default();
    let project = project(&podman, paths, folder)?;
    let agent = match agent {
        Some(named) => agent_for(&podman, &project, named)?,
        None => String::new(),
    };
    let home: PathBuf = home_dir()?;
    // Serialized with session starts, which rewrite the proxy's rules too.
    let _lock = pod::lock(&paths.state).map_err(|e| e.to_string())?;
    let policy = paths.load_policy()?;
    let running = podman
        .exists(Object::Pod, &project.pod())
        .map_err(|e| e.to_string())?;
    let target = approvals::Project {
        checkout: &project.checkout,
        agent: &agent,
        id: &project.id,
        policy_file: &paths.policy,
        home: &home,
        state_dir: &paths.state,
    };
    let outcome = apply(&target, &policy, running);
    let services = Services::new(&podman, paths);
    if podman
        .exists(Object::Container, &services.proxy.service.container)
        .map_err(|e| e.to_string())?
    {
        services
            .rules(&paths.load_policy()?, paths)
            .map_err(|e| format!("the change is saved, but the proxy didn't apply it: {e}. Run the same command again to retry"))?;
    }
    for message in outcome? {
        eprintln!("agent-run: {message}");
    }
    Ok(())
}

pub fn allow(
    folder: &Path,
    value: &str,
    scope: Scope,
    agent: Option<&str>,
    paths: &Paths,
) -> Result<(), String> {
    let destination = approvals::parse_destination(value).map_err(|e| e.to_string())?;
    change(paths, folder, Some(agent), |target, policy, running| {
        approvals::allow(target, policy, &destination, scope, running)
            .map(|(_, messages)| messages)
            .map_err(|e| e.to_string())
    })
}

pub fn revoke(
    folder: &Path,
    value: &str,
    global: bool,
    agent: Option<&str>,
    paths: &Paths,
) -> Result<(), String> {
    let destination = approvals::parse_destination(value).map_err(|e| e.to_string())?;
    // The shared list isn't an agent's: no agent to find.
    let agent = if global { None } else { Some(agent) };
    change(paths, folder, agent, |target, policy, _| {
        approvals::revoke(target, policy, &destination, global).map_err(|e| e.to_string())
    })
}

/// The project's recently refused destinations, newest first, with the agent (or build).
pub fn denied(folder: &Path, paths: &Paths) -> Result<(), String> {
    let podman = Podman::default();
    let project = project(&podman, paths, folder)?;
    if !podman
        .exists(Object::Network, &project.network())
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "no pod runs for {}; refusals are matched by its pod's network",
            project.checkout.display()
        ));
    }
    let subnet = egress::subnet(&podman, &project.network()).map_err(|e| e.to_string())?;
    let log = Services::new(&podman, paths)
        .proxy
        .service
        .log()
        .map_err(|e| e.to_string())?;
    let denials = egress::denials(&log, &subnet, 20);
    if denials.is_empty() {
        println!("Nothing refused for {}.", project.checkout.display());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    print!("{}", egress::report(&denials, now));
    Ok(())
}

/// What the policy grants: the shared list, then each agent's own destinations and approvals.
pub fn show(folder: &Path, paths: &Paths) -> Result<(), String> {
    let project = project(&Podman::default(), paths, folder)?;
    let policy = paths.load_policy()?;
    print!("{}", policy.describe(&project.checkout, &[]));
    for agent in agents::Agent::ALL {
        let grants = policy.for_agent(&project.checkout, agent.name());
        let session = approvals::Session::new(&paths.state, &project.id, agent.name())
            .list()
            .map_err(|e| e.to_string())?;
        let list = |d: &[policy::Destination]| {
            if d.is_empty() {
                "none".to_owned()
            } else {
                d.iter().map(|d| d.short()).collect::<Vec<_>>().join(", ")
            }
        };
        println!(
            "{}\n  reaches:           {}\n  local:             {}\n  this session:      {}",
            agent.name(),
            list(&grants.allow),
            list(&grants.local),
            list(&session)
        );
    }
    Ok(())
}
