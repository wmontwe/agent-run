//! agent-run's policy: what agents may reach and change, in one TOML file
//! (`~/.agent-run/policy.toml`, see docs/configuration.md).
//!
//! The file is parsed strictly (unknown keys are errors) and validated into typed
//! settings. Everything a container could influence is out of scope here: the policy
//! is never read from a location a container can write.

mod describe;
mod raw;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Validated policy.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    /// Destinations every project pod may reach (agent and build containers together).
    pub project_allow: Vec<Destination>,
    /// Repositories the GitHub relay serves, `owner/repo`.
    pub github_repos: Vec<String>,
    /// Maven-style remotes the mirror proxies, by repository id.
    pub mirror_remotes: BTreeMap<String, String>,
    pub workspace: WorkspaceRules,
    /// Memory limit of each agent session's container, in bytes.
    pub agent_memory: u64,
    /// CPUs of each agent session's container, on top of the VM's own limit.
    pub agent_cpus: f64,
    /// Builds a project runs at once; further `gradle-run` calls wait.
    pub max_concurrent_builds: u32,
    /// Memory limit of a project's build container, in bytes.
    pub build_memory: u64,
    /// CPUs of a project's build container, on top of the VM's own limit.
    pub build_cpus: f64,
    /// Per-project additions, by canonical-looking absolute checkout path.
    pub projects: BTreeMap<PathBuf, ProjectSettings>,
    /// Each agent's own destinations, by agent name, for every project.
    pub agents: BTreeMap<String, Grants>,
}

/// What one agent reaches besides the project's shared list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    pub allow: Vec<Destination>,
    /// Exact destinations on the Mac or in the local network, plain HTTP included.
    pub local: Vec<Destination>,
}

/// Folder rules for the checkout, as exact paths relative to its root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceRules {
    pub mask: Vec<String>,
    pub read_only: Vec<String>,
    pub private: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectSettings {
    pub allow: Vec<Destination>,
    pub build_memory: Option<u64>,
    /// Each agent's own approvals in this project.
    pub agents: BTreeMap<String, Grants>,
    /// The project's engram package, declared, never detected.
    pub engram: Option<PathBuf>,
}

/// A host and port an egress proxy may connect to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Destination {
    pub host: String,
    pub port: u16,
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// What a session in one project gets: the global settings plus the project's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectView<'a> {
    pub allow: Vec<&'a Destination>,
    pub build_memory: u64,
    pub engram: Option<&'a Path>,
}

impl Policy {
    /// What `agent` reaches in the project at `checkout` besides the shared list: its own
    /// destinations and the project's approvals for it.
    pub fn for_agent(&self, checkout: &Path, agent: &str) -> Grants {
        let mut grants = self.agents.get(agent).cloned().unwrap_or_default();
        if let Some(project) = self
            .projects
            .get(checkout)
            .and_then(|p| p.agents.get(agent))
        {
            grants.allow.extend(project.allow.iter().cloned());
            grants.local.extend(project.local.iter().cloned());
        }
        for list in [&mut grants.allow, &mut grants.local] {
            list.sort();
            list.dedup();
        }
        grants
    }

    pub fn for_project(&self, checkout: &Path) -> ProjectView<'_> {
        let project = self.projects.get(checkout);
        let mut allow: Vec<&Destination> = self.project_allow.iter().collect();
        if let Some(project) = project {
            allow.extend(project.allow.iter());
        }
        allow.sort();
        allow.dedup();
        ProjectView {
            allow,
            build_memory: project
                .and_then(|p| p.build_memory)
                .unwrap_or(self.build_memory),
            engram: project.and_then(|p| p.engram.as_deref()),
        }
    }
}

/// Why a policy was refused. The message names the offending key and value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyError(pub String);

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PolicyError {}

fn error(message: impl Into<String>) -> PolicyError {
    PolicyError(message.into())
}

const DEFAULT_AGENT_MEMORY: u64 = 2 << 30;
const DEFAULT_BUILD_MEMORY: u64 = 16 << 30;
const DEFAULT_AGENT_CPUS: f64 = 2.0;
const DEFAULT_BUILD_CPUS: f64 = 8.0;

/// A CPU limit: more than 0, at most 64.
fn cpus(value: Option<f64>, default: f64, key: &str) -> Result<f64, PolicyError> {
    match value {
        None => Ok(default),
        Some(n) if n > 0.0 && n <= 64.0 => Ok(n),
        Some(n) => Err(error(format!(
            "{key}: {n} must be more than 0 and at most 64"
        ))),
    }
}

/// Reads the policy file; `home` expands `~/` in project and engram paths.
pub fn load(path: &Path, home: &Path) -> Result<Policy, PolicyError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| error(format!("{}: {e}", path.display())))?;
    parse(&text, home).map_err(|e| error(format!("{}: {e}", path.display())))
}

pub fn parse(text: &str, home: &Path) -> Result<Policy, PolicyError> {
    let raw: raw::Policy =
        toml::from_str(text).map_err(|e| error(e.message().to_owned() + &location(text, &e)))?;
    if raw.version != 1 {
        return Err(error(format!("version: expected 1, found {}", raw.version)));
    }
    let network = raw.network.unwrap_or_default();
    let workspace = raw.workspace.unwrap_or_default();
    let agents = raw.agents.unwrap_or_default();
    let builds = raw.builds.unwrap_or_default();

    let mut projects = BTreeMap::new();
    for (key, project) in raw.projects {
        let path = absolute(&key, home).map_err(|e| error(format!("projects.\"{key}\": {e}")))?;
        // By its real path, as checkouts are looked up and approvals edit it: one project, one
        // identity, whatever spelling (symlinks) the policy uses. A path that doesn't exist
        // yet stays as written.
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        let settings = ProjectSettings {
            allow: destinations(
                &project
                    .network
                    .and_then(|n| n.project)
                    .map(|p| p.allow)
                    .unwrap_or_default(),
                &format!("projects.\"{key}\".network.project.allow"),
            )?,
            build_memory: project
                .builds
                .and_then(|b| b.memory)
                .map(|m| memory(&m, &format!("projects.\"{key}\".builds.memory")))
                .transpose()?,
            engram: project
                .engram
                .map(|e| {
                    absolute(&e, home).map_err(|m| error(format!("projects.\"{key}\".engram: {m}")))
                })
                .transpose()?,
            agents: grants(project.agents, &format!("projects.\"{key}\".agents"))?,
        };
        if projects.insert(path.clone(), settings).is_some() {
            return Err(error(format!("projects: {} appears twice", path.display())));
        }
    }

    Ok(Policy {
        project_allow: destinations(
            &network.project.map(|p| p.allow).unwrap_or_default(),
            "network.project.allow",
        )?,
        github_repos: repos(&network.github.map(|g| g.repos).unwrap_or_default())?,
        mirror_remotes: remotes(raw.mirror.map(|m| m.remotes).unwrap_or_default())?,
        workspace: WorkspaceRules {
            mask: folders(&workspace.mask, "workspace.mask")?,
            read_only: folders(&workspace.read_only, "workspace.read-only")?,
            private: folders(&workspace.private, "workspace.private")?,
        },
        agent_memory: agents
            .memory
            .map(|m| memory(&m, "agents.memory"))
            .transpose()?
            .unwrap_or(DEFAULT_AGENT_MEMORY),
        agent_cpus: cpus(agents.cpus, DEFAULT_AGENT_CPUS, "agents.cpus")?,
        build_cpus: cpus(builds.cpus, DEFAULT_BUILD_CPUS, "builds.cpus")?,
        max_concurrent_builds: match builds.max_concurrent {
            Some(0) => return Err(error("builds.max-concurrent: must be at least 1")),
            Some(n) => n,
            None => 1,
        },
        build_memory: builds
            .memory
            .map(|m| memory(&m, "builds.memory"))
            .transpose()?
            .unwrap_or(DEFAULT_BUILD_MEMORY),
        projects,
        agents: grants(agents.named, "agents")?,
    })
}

/// `[<prefix>.<agent>]` tables: known agent names, hosts in `allow`, exact local destinations in
/// `local`.
fn grants(
    tables: BTreeMap<String, raw::AgentNetwork>,
    prefix: &str,
) -> Result<BTreeMap<String, Grants>, PolicyError> {
    let mut out = BTreeMap::new();
    for (name, table) in tables {
        if agents::Agent::parse(&name).is_none() {
            let known: Vec<&str> = agents::Agent::ALL.iter().map(|a| a.name()).collect();
            return Err(error(format!(
                "{prefix}.{name}: unknown agent; known are {}",
                known.join(", ")
            )));
        }
        let grants = Grants {
            allow: destinations(&table.allow, &format!("{prefix}.{name}.allow"))?,
            local: locals(&table.local, &format!("{prefix}.{name}.local"))?,
        };
        out.insert(name, grants);
    }
    Ok(out)
}

/// Exact local destinations: `host:port`, the host an IPv4 address or a host name such as
/// `host.containers.internal`; the port required. No ranges or wildcards.
fn locals(values: &[String], key: &str) -> Result<Vec<Destination>, PolicyError> {
    let mut parsed = Vec::new();
    for value in values {
        let invalid = |why: &str| error(format!("{key}: \"{value}\" {why}"));
        let (host, port) = value
            .rsplit_once(':')
            .ok_or_else(|| invalid("needs a port, such as host.containers.internal:11434"))?;
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| invalid("has an invalid port"))?;
        let name_ok = host.split('.').all(|l| {
            !l.is_empty()
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        });
        if host.parse::<std::net::Ipv4Addr>().is_err() && !name_ok {
            return Err(invalid(
                "must be an IPv4 address or a lowercase host name, with a port",
            ));
        }
        parsed.push(Destination {
            host: host.to_owned(),
            port,
        });
    }
    Ok(parsed)
}

fn location(text: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => format!(" (line {})", text[..span.start].lines().count().max(1)),
        None => String::new(),
    }
}

/// Exact folder paths relative to the checkout root: no globs, no `.` or `..`, no
/// absolute paths, no trailing slash. Folder rules can't name single files on macOS
/// (case aliases bypass them), but whether a path is a folder is checked against the
/// checkout, not here.
fn folders(values: &[String], key: &str) -> Result<Vec<String>, PolicyError> {
    let mut seen = Vec::new();
    for value in values {
        let invalid = |why: &str| error(format!("{key}: \"{value}\" {why}"));
        if value.is_empty() || value.starts_with('/') || value.ends_with('/') {
            return Err(invalid(
                "must be a relative folder path without leading or trailing slash",
            ));
        }
        if value.contains(['*', '?', '[', ']', '{', '}', '\\']) {
            return Err(invalid("must be an exact path; globs aren't supported"));
        }
        if value
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(invalid("must not contain empty, `.` or `..` segments"));
        }
        if seen.contains(value) {
            return Err(invalid("appears twice"));
        }
        seen.push(value.clone());
    }
    Ok(seen)
}

/// `host` or `host:port`; port 443 by default. Hosts are lowercase DNS names: literal
/// IP addresses and wildcards aren't allowed here.
fn destinations(values: &[String], key: &str) -> Result<Vec<Destination>, PolicyError> {
    let mut parsed = Vec::new();
    for value in values {
        let invalid = |why: &str| error(format!("{key}: \"{value}\" {why}"));
        let (host, port) = match value.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse::<u16>()
                    .ok()
                    .filter(|p| *p > 0)
                    .ok_or_else(|| invalid("has an invalid port"))?,
            ),
            None => (value.as_str(), 443),
        };
        let label_ok = |label: &str| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        let labels: Vec<&str> = host.split('.').collect();
        if labels.len() < 2
            || !labels.iter().all(|l| label_ok(l))
            || labels
                .last()
                .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(invalid(
                "must be a lowercase host name such as api.example.com, optionally with :port",
            ));
        }
        let destination = Destination {
            host: host.to_owned(),
            port,
        };
        if parsed.contains(&destination) {
            return Err(invalid("appears twice"));
        }
        parsed.push(destination);
    }
    Ok(parsed)
}

/// One destination as the policy writes it: `host` or `host:port`, port 443 by default.
pub fn destination(value: &str) -> Result<Destination, PolicyError> {
    let mut parsed = destinations(&[value.to_owned()], "destination")?;
    Ok(parsed.remove(0))
}

/// The checkout path a `projects."<key>"` table names: absolute, or `~/` expanded with `home`.
pub fn project_path(key: &str, home: &Path) -> Result<PathBuf, PolicyError> {
    absolute(key, home).map_err(|e| error(format!("projects.\"{key}\": {e}")))
}

fn repos(values: &[String]) -> Result<Vec<String>, PolicyError> {
    let part_ok = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    for value in values {
        match value.split_once('/') {
            Some((owner, repo)) if part_ok(owner) && part_ok(repo) => {}
            _ => {
                return Err(error(format!(
                    "network.github.repos: \"{value}\" must be owner/repo"
                )));
            }
        }
    }
    Ok(values.to_vec())
}

fn remotes(values: BTreeMap<String, String>) -> Result<BTreeMap<String, String>, PolicyError> {
    for (id, url) in &values {
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(error(format!(
                "mirror.remotes: \"{id}\" must be lowercase letters, digits and dashes"
            )));
        }
        let rest = url.strip_prefix("https://").ok_or_else(|| {
            error(format!(
                "mirror.remotes.{id}: \"{url}\" must be an https URL"
            ))
        })?;
        if rest.is_empty()
            || rest.starts_with('/')
            || rest.contains(['?', '#', ' '])
            || rest.ends_with('/')
        {
            return Err(error(format!(
                "mirror.remotes.{id}: \"{url}\" must be https://host/path without a trailing slash"
            )));
        }
    }
    Ok(values)
}

/// Sizes such as `512m`, `2g` or `16g`, in bytes.
fn memory(value: &str, key: &str) -> Result<u64, PolicyError> {
    let invalid = || {
        error(format!(
            "{key}: \"{value}\" must be a size such as 512m or 2g"
        ))
    };
    let (number, shift) = match value.as_bytes().last() {
        Some(b'k') => (&value[..value.len() - 1], 10),
        Some(b'm') => (&value[..value.len() - 1], 20),
        Some(b'g') => (&value[..value.len() - 1], 30),
        _ => return Err(invalid()),
    };
    let number: u64 = number.parse().ok().filter(|n| *n > 0).ok_or_else(invalid)?;
    number.checked_mul(1 << shift).ok_or_else(invalid)
}

/// An absolute path, or `~/...` expanded with `home`. Not canonicalized: the policy
/// may name paths that don't exist yet; callers compare against canonical paths.
fn absolute(value: &str, home: &Path) -> Result<PathBuf, String> {
    let path = match value.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if value.starts_with('/') => PathBuf::from(value),
        None => {
            return Err(format!(
                "\"{value}\" must be an absolute path or start with ~/"
            ));
        }
    };
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(format!("\"{value}\" must not contain . or .."));
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
