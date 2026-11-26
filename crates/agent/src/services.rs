//! The shared services every project pod uses: the egress proxy, the GitHub relay and the
//! mirror. The one place that knows how they connect: the mirror downloads through the proxy,
//! and the proxy's rules cover every pod and the mirror.

use crate::Paths;
use crate::images;
use egress::{Egress, PodEgress};
use mirror::Mirror;
use pod::Project;
use podman::{Podman, Service};
use policy::{Destination, Policy};
use relay::Relay;

#[cfg(test)]
mod tests;

pub struct Services<'a> {
    pub proxy: Egress<'a>,
    pub relay: Relay<'a>,
    pub mirror: Mirror<'a>,
    podman: &'a Podman,
}

impl<'a> Services<'a> {
    pub fn new(podman: &'a Podman, paths: &Paths) -> Services<'a> {
        let (ns, state) = (&paths.namespace, &paths.state);
        Services {
            proxy: Egress {
                service: egress::service(podman, ns, state),
                image: images::PROXY,
                mirror: Some((mirror::ALIAS, mirror::PORT)),
            },
            relay: Relay {
                service: relay::service(podman, ns, state),
                image: images::RELAY,
            },
            mirror: Mirror {
                service: mirror::service(podman, ns, state),
                volume: ns.name("mirror-data"),
                image: images::MIRROR,
                tls: mirror_tls(paths),
            },
            podman,
        }
    }

    fn all(&self) -> [&Service<'a>; 3] {
        [
            &self.proxy.service,
            &self.relay.service,
            &self.mirror.service,
        ]
    }

    /// Starts every service with the policy's settings and joins them to a pod's network.
    pub fn start(&self, policy: &Policy, paths: &Paths, network: &str) -> Result<(), String> {
        let err = |e: podman::PodmanError| e.to_string();
        self.mirror.service.network().map_err(err)?;
        self.rules(paths)?;
        self.proxy
            .service
            .connect(&self.mirror.service.network)
            .map_err(err)?;
        let proxy = self
            .proxy
            .service
            .address_on(&self.mirror.service.network)
            .map_err(err)?;
        self.mirror
            .ensure(&policy.mirror_remotes, &format!("{proxy}:{}", egress::PORT))
            .map_err(err)?;
        self.all()
            .iter()
            .try_for_each(|s| s.connect(network))
            .map_err(err)
    }

    /// Writes the proxy's rules: each of the namespace's pods gets its project's shared list,
    /// and each running session its agent's destinations and approvals in the project; the
    /// mirror its remotes' hosts. The relay gets the global repositories and each pod's own.
    pub fn rules(&self, paths: &Paths) -> Result<(), String> {
        // Session teardown must not restore grants from that session's startup snapshot.
        let policy = paths.load_policy()?;
        let err = |e: &dyn std::fmt::Display| e.to_string();
        let mut pods = Vec::new();
        let mut relay_pods = Vec::new();
        let mut passwords = Vec::new();
        for checkout in pod::checkouts(self.podman, &paths.namespace).map_err(|e| err(&e))? {
            let project = Project::new(&paths.namespace, &checkout);
            let allow: Vec<Destination> = policy
                .for_project(&checkout)
                .allow
                .into_iter()
                .cloned()
                .collect();
            let mut sessions = Vec::new();
            for (user, agent) in session_identities(self.podman, &project)? {
                let Ok(line) = std::fs::read_to_string(credentials(paths, &user)) else {
                    continue;
                };
                let mut grants = policy.for_agent(&checkout, &agent);
                grants.allow.extend(
                    approvals::Session::new(&paths.state, &project.id, &agent)
                        .list()
                        .map_err(|e| err(&e))?,
                );
                grants.allow.sort();
                grants.allow.dedup();
                passwords.push(line.trim().to_owned());
                sessions.push(egress::SessionEgress {
                    user,
                    allow: grants.allow,
                    local: grants.local,
                });
            }
            if let Some(identity) = rules_build_identity(&build_credentials(
                paths,
                &project,
                &build_generation(self.podman, &project)?,
            )) {
                passwords.push(identity.password_line);
                sessions.push(egress::SessionEgress {
                    user: identity.user,
                    allow: policy.for_build(&checkout),
                    local: vec![],
                });
            }
            let subnet = egress::subnet(self.podman, &project.network()).map_err(|e| err(&e))?;
            let mut repos = policy
                .projects
                .get(&checkout)
                .map(|p| p.github_repos.clone())
                .unwrap_or_default();
            repos.extend(
                approvals::SessionRepos::new(&paths.state, &project.id)
                    .list()
                    .map_err(|e| err(&e))?,
            );
            relay_pods.push(relay::PodRepos {
                subnet: subnet.clone(),
                repos,
            });
            pods.push(PodEgress {
                subnet,
                allow,
                sessions,
            });
        }
        if self
            .podman
            .exists(podman::Object::Network, &self.mirror.service.network)
            .map_err(|e| err(&e))?
        {
            let allow = mirror::hosts(&policy.mirror_remotes)
                .into_iter()
                .map(|host| Destination { host, port: 443 })
                .collect();
            pods.push(PodEgress {
                subnet: egress::subnet(self.podman, &self.mirror.service.network)
                    .map_err(|e| err(&e))?,
                allow,
                sessions: vec![],
            });
        }
        self.proxy.ensure(&pods, &passwords).map_err(|e| err(&e))?;
        // The relay's per-project repositories follow the same pods and the same fresh policy.
        self.relay
            .ensure(&policy.github_repos, &relay_pods)
            .map_err(|e| err(&e))
    }

    /// A new session identity for `agent`: a random user and password, their htpasswd line kept
    /// while the session runs. Returns the user and the session's proxy URL.
    pub fn identity(paths: &Paths, agent: &str) -> Result<(String, String), String> {
        let identity = egress::Identity::new(agent)?;
        let file = credentials(paths, &identity.user);
        std::fs::create_dir_all(file.parent().expect("in a folder")).map_err(|e| e.to_string())?;
        std::fs::write(&file, &identity.password_line)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        Ok((identity.user, identity.url))
    }

    /// A pod's build credentials are stable across sessions and never mounted into agents.
    pub fn build_identity(
        podman: &Podman,
        paths: &Paths,
        project: &Project,
    ) -> Result<egress::Identity, String> {
        build_identity_for_generation(paths, project, &build_generation(podman, project)?)
    }

    pub fn forget_build(paths: &Paths, project: &Project) {
        let _ = std::fs::remove_dir_all(paths.state.join("build-proxies").join(&project.id));
    }

    /// Ends a session's identity.
    pub fn forget(paths: &Paths, user: &str) {
        let _ = std::fs::remove_file(credentials(paths, user));
    }

    /// Every proxy identity, once no sessions run.
    pub fn forget_all(paths: &Paths) {
        let _ = std::fs::remove_dir_all(paths.state.join("proxy-users"));
        let _ = std::fs::remove_dir_all(paths.state.join("build-proxies"));
    }

    /// Takes every service off a pod's network, so the network can be removed.
    pub fn leave(&self, network: &str) -> Result<(), String> {
        self.all()
            .iter()
            .try_for_each(|s| s.disconnect(network))
            .map_err(|e| e.to_string())
    }
}

/// A pod's saved build identity, for the proxy rules of every project. One that doesn't load
/// (written by an older agent-run, or damaged) costs only its own pod the build-only hosts, until
/// that project is restarted; it mustn't stop the rules of the others, or `agent restart` itself.
fn rules_build_identity(path: &std::path::Path) -> Option<egress::Identity> {
    match egress::Identity::load(path, "build") {
        Ok(identity) => identity,
        Err(e) => {
            eprintln!("agent-run: warning: {e}");
            None
        }
    }
}

fn build_identity_for_generation(
    paths: &Paths,
    project: &Project,
    generation: &str,
) -> Result<egress::Identity, String> {
    let path = build_credentials(paths, project, generation);
    if let Some(identity) = egress::Identity::load(&path, "build")? {
        return Ok(identity);
    }
    let identity = egress::Identity::new("build")?;
    identity.save(&path)?;
    Ok(identity)
}

fn build_generation(podman: &Podman, project: &Project) -> Result<String, String> {
    let generation = podman
        .run(&["pod", "inspect", "--format", "{{.Id}}", &project.pod()])
        .map_err(|e| e.to_string())?;
    let generation = generation.trim();
    if generation.len() != 64 || !generation.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "{}: Podman returned an invalid pod ID",
            project.pod()
        ));
    }
    Ok(generation.to_owned())
}

/// Labels on a session's container: its agent and its proxy user.
pub const AGENT_LABEL: &str = "agent-run.agent";
pub const PROXY_USER_LABEL: &str = "agent-run.proxy-user";

fn build_credentials(paths: &Paths, project: &Project, generation: &str) -> std::path::PathBuf {
    paths
        .state
        .join("build-proxies")
        .join(&project.id)
        .join(generation)
}

fn credentials(paths: &Paths, user: &str) -> std::path::PathBuf {
    paths.state.join("proxy-users").join(user)
}

/// The running agent sessions of a project's pod: their proxy user and agent.
/// Build identities use a separate label and must never count as active agent sessions.
pub fn session_identities(
    podman: &Podman,
    project: &Project,
) -> Result<Vec<(String, String)>, String> {
    let filter = format!("pod={}", project.pod());
    let names = podman
        .run(&["ps", "--filter", &filter, "--format", "{{.Names}}"])
        .map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    for name in names.lines().filter(|n| !n.is_empty()) {
        let labels = podman
            .labels(podman::Object::Container, name)
            .map_err(|e| e.to_string())?;
        if let (Some(user), Some(agent)) = (labels.get(PROXY_USER_LABEL), labels.get(AGENT_LABEL)) {
            found.push((user.clone(), agent.clone()));
        }
    }
    Ok(found)
}

/// The mirror's certificate authority, in host state: the mirror serves with it, build
/// containers trust it.
pub fn mirror_tls(paths: &Paths) -> mirror::Tls {
    mirror::Tls::new(paths.state.join("mirror-tls"))
}
