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
        self.rules(policy, paths)?;
        self.proxy
            .service
            .connect(&self.mirror.service.network)
            .map_err(err)?;
        self.relay.ensure(&policy.github_repos).map_err(err)?;
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
    /// mirror its remotes' hosts.
    pub fn rules(&self, policy: &Policy, paths: &Paths) -> Result<(), String> {
        let err = |e: &dyn std::fmt::Display| e.to_string();
        let mut pods = Vec::new();
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
            pods.push(PodEgress {
                subnet: egress::subnet(self.podman, &project.network()).map_err(|e| err(&e))?,
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
        self.proxy.ensure(&pods, &passwords).map_err(|e| err(&e))
    }

    /// A new session identity for `agent`: a random user and password, their htpasswd line kept
    /// while the session runs. Returns the user and the session's proxy URL.
    pub fn identity(paths: &Paths, agent: &str) -> Result<(String, String), String> {
        let random = |bytes: usize| -> Result<String, String> {
            let mut buffer = vec![0u8; bytes];
            std::io::Read::read_exact(
                &mut std::fs::File::open("/dev/urandom").map_err(|e| e.to_string())?,
                &mut buffer,
            )
            .map_err(|e| e.to_string())?;
            Ok(buffer.iter().map(|b| format!("{b:02x}")).collect())
        };
        let (password, salt) = (random(16)?, random(4)?);
        let user = format!("{agent}-{}", random(6)?);
        let file = credentials(paths, &user);
        std::fs::create_dir_all(file.parent().expect("in a folder")).map_err(|e| e.to_string())?;
        std::fs::write(&file, egress::htpasswd_line(&user, &password, &salt))
            .map_err(|e| format!("{}: {e}", file.display()))?;
        let url = format!(
            "http://{user}:{password}@{}:{}",
            egress::ALIAS,
            egress::PORT
        );
        Ok((user, url))
    }

    /// Ends a session's identity.
    pub fn forget(paths: &Paths, user: &str) {
        let _ = std::fs::remove_file(credentials(paths, user));
    }

    /// Every session's proxy credentials, once no session runs.
    pub fn forget_all(paths: &Paths) {
        let _ = std::fs::remove_dir_all(paths.state.join("proxy-users"));
    }

    /// Takes every service off a pod's network, so the network can be removed.
    pub fn leave(&self, network: &str) -> Result<(), String> {
        self.all()
            .iter()
            .try_for_each(|s| s.disconnect(network))
            .map_err(|e| e.to_string())
    }
}

/// Labels on a session's container: its agent and its proxy user.
pub const AGENT_LABEL: &str = "agent-run.agent";
pub const PROXY_USER_LABEL: &str = "agent-run.proxy-user";

fn credentials(paths: &Paths, user: &str) -> std::path::PathBuf {
    paths.state.join("proxy-users").join(user)
}

/// The running sessions of a project's pod: their proxy user and agent.
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
