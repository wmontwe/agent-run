//! agent-run's launcher as a library: the steps the `agent` command runs, in order, usable by
//! the integration tests without a terminal. Each step's logic lives in its own crate.

pub mod approve;
pub mod images;
pub mod services;
pub mod session;
pub mod signals;

use podman::Namespace;
use policy::Policy;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Where agent-run's configuration and state live, and the namespace of its Podman objects.
pub struct Paths {
    pub policy: PathBuf,
    pub state: PathBuf,
    pub agent_home: PathBuf,
    pub instructions: PathBuf,
    pub knowledge: PathBuf,
    pub namespace: Namespace,
}

impl Paths {
    pub fn new(base: &Path, agent: &str) -> Paths {
        Paths {
            policy: base.join("policy.toml"),
            state: base.join("state"),
            agent_home: base.join(agent).join("home"),
            instructions: base.join("instructions.md"),
            knowledge: base.join("knowledge"),
            namespace: Namespace::default(),
        }
    }

    /// `~/.agent-run`, or `$AGENT_RUN_HOME`; Podman objects in `$AGENT_RUN_NAMESPACE` if set
    /// (tests of the public command).
    pub fn from_env(agent: &str) -> Result<Paths, String> {
        let base = match std::env::var_os("AGENT_RUN_HOME") {
            Some(base) => PathBuf::from(base),
            None => home_dir()?.join(".agent-run"),
        };
        let mut paths = Paths::new(&base, agent);
        if let Ok(namespace) = std::env::var("AGENT_RUN_NAMESPACE") {
            paths.namespace = podman::Namespace::new(&namespace);
        }
        Ok(paths)
    }

    pub fn load_policy(&self) -> Result<Policy, String> {
        if !self.policy.exists() {
            return Err(format!(
                "no policy at {}; create it using the README quick start or docs/configuration.md",
                self.policy.display()
            ));
        }
        policy::load(&self.policy, &home_dir()?).map_err(|e| e.to_string())
    }
}

/// Prints each step with its duration, so a slow start is visible.
pub struct Progress {
    started: Instant,
}

impl Progress {
    pub fn start() -> Progress {
        Progress {
            started: Instant::now(),
        }
    }

    pub fn step<T>(
        &mut self,
        name: &str,
        f: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let started = Instant::now();
        // Nothing more is acquired once the launcher was told to stop.
        if crate::signals::stopped() {
            return Err(format!(
                "stopped before {name}: the launcher was told to stop"
            ));
        }
        let mut result = f();
        // For tests of failed starts: fail right after the named step succeeded.
        if result.is_ok() && std::env::var("AGENT_RUN_FAIL_AFTER").as_deref() == Ok(name) {
            result = Err(format!("failure injected after {name}"));
        }
        let status = if result.is_ok() { "" } else { " failed" };
        eprintln!(
            "agent-run: {name}{status} ({} ms)",
            started.elapsed().as_millis()
        );
        result
    }

    pub fn total(&self) -> u128 {
        self.started.elapsed().as_millis()
    }
}

pub fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_owned())
}

/// The repository this launcher was built from: the image sources live there.
pub fn repository() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}
