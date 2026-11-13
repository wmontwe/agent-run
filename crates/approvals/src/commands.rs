//! What `agent allow` and `agent revoke` decide: which list changes, and what to tell the
//! user. The caller finds the project, holds the start lock and reloads the proxy.

use crate::{
    ApprovalError, List, Session, edit_policy, error, policy_add, policy_contains, policy_remove,
    written,
};
use policy::{Destination, Policy};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Session,
    Project,
}

/// The project an approval is for, and where its approvals live.
pub struct Project<'a> {
    pub checkout: &'a Path,
    /// The agent the approval is for: approvals are never shared between agents.
    pub agent: &'a str,
    pub id: &'a str,
    pub policy_file: &'a Path,
    pub home: &'a Path,
    pub state_dir: &'a Path,
}

/// `host` or `host:port` as the user typed it.
pub fn parse_destination(value: &str) -> Result<Destination, ApprovalError> {
    policy::destination(value).map_err(|e| {
        error(format!("{e}. Wildcards and IP addresses can't be approved with this command; edit the policy if you really need one"))
    })
}

/// Approves `destination`; returns whether anything changed and what to tell the user.
pub fn allow(
    project: &Project,
    policy: &Policy,
    destination: &Destination,
    scope: Scope,
    pod_running: bool,
) -> Result<(bool, Vec<String>), ApprovalError> {
    let checkout = project.checkout.display();
    let agent = project.agent;
    if policy
        .for_project(project.checkout)
        .allow
        .contains(&destination)
        || policy
            .for_agent(project.checkout, agent)
            .allow
            .contains(destination)
    {
        return Ok((
            false,
            vec![format!(
                "{destination} is already allowed for {agent} in {checkout} by the policy"
            )],
        ));
    }
    let mut messages = Vec::new();
    let changed = match scope {
        Scope::Session if !pod_running => {
            return Err(error(format!(
                "no session runs in {checkout}; a session approval lasts until its pod is removed. Use --project to keep it in the policy"
            )));
        }
        Scope::Session => Session::new(project.state_dir, project.id, agent).add(destination)?,
        Scope::Project => {
            let list = List::Agent {
                checkout: project.checkout,
                home: project.home,
                agent,
            };
            let changed = edit_policy(project.policy_file, project.home, |text| {
                policy_add(text, list, destination)
            })?;
            if changed && !policy.projects.contains_key(project.checkout) {
                messages.push(format!(
                    "note: adds a table for {checkout} to {}",
                    project.policy_file.display()
                ));
            }
            changed
        }
    };
    if !changed {
        return Ok((
            false,
            vec![format!(
                "{destination} is already approved for {agent} in {checkout}"
            )],
        ));
    }
    if destination.port != 443 {
        messages.push(format!(
            "note: port {}, not HTTPS's usual 443",
            destination.port
        ));
    }
    let lasts = if scope == Scope::Session {
        "until the pod is removed"
    } else {
        "in the policy"
    };
    messages.push(format!(
        "grants {agent} in {checkout} access to {destination}, {lasts}; no other agent"
    ));
    Ok((true, messages))
}

/// Withdraws `destination` for the project (its session and policy approvals), or with
/// `global` from every project's list; returns what to tell the user.
pub fn revoke(
    project: &Project,
    policy: &Policy,
    destination: &Destination,
    global: bool,
) -> Result<Vec<String>, ApprovalError> {
    let mut messages = Vec::new();
    if global {
        if !policy.project_allow.contains(destination) {
            return Err(error(format!(
                "{destination} isn't on the global list (network.project.allow)"
            )));
        }
        messages.push(format!(
            "removes {destination} from every project's allowlist"
        ));
        let keep: Vec<String> = policy
            .projects
            .iter()
            .filter(|(_, p)| p.allow.contains(destination))
            .map(|(path, _)| path.display().to_string())
            .collect();
        if !keep.is_empty() {
            messages.push(format!(
                "still allowed by their own tables: {}",
                keep.join(", ")
            ));
        }
        edit_policy(project.policy_file, project.home, |text| {
            policy_remove(text, List::Global, destination)
        })?;
    } else {
        let from_session =
            Session::new(project.state_dir, project.id, project.agent).remove(destination)?;
        let list = List::Agent {
            checkout: project.checkout,
            home: project.home,
            agent: project.agent,
        };
        let from_policy = edit_policy(project.policy_file, project.home, |text| {
            policy_remove(text, list, destination)
        })?;
        let text = std::fs::read_to_string(project.policy_file)
            .map_err(|e| error(format!("{}: {e}", project.policy_file.display())))?;
        let global = policy_contains(&text, List::Global, destination)?;
        if !from_session && !from_policy {
            return Err(error(if global {
                format!(
                    "{destination} is on the global list, for every project; `agent revoke --global {}` removes it there",
                    written(destination)
                )
            } else {
                format!(
                    "{destination} isn't approved for {} in {}",
                    project.agent,
                    project.checkout.display()
                )
            }));
        }
        messages.push(format!(
            "revoked {destination} for {} in {}",
            project.agent,
            project.checkout.display()
        ));
        if global {
            messages.push(
                "note: still allowed by the global list; `agent revoke --global` removes it there"
                    .into(),
            );
        }
    }
    messages.push("new connections are refused; open ones stay until they close".into());
    Ok(messages)
}
