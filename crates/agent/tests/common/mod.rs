//! Shared by the launcher's integration tests: a scratch agent-run home and a Podman
//! namespace of its own, both removed when the test ends, whatever happened.

#![allow(dead_code)]

use agent::Paths;
use podman::{Namespace, Podman};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Scratch {
    pub base: PathBuf,
    pub namespace: Namespace,
    /// Volumes kept after the test (names ending so), such as a mirror's cache.
    pub keep: Vec<&'static str>,
}

impl Scratch {
    pub fn new(test: &str) -> Scratch {
        let base = temp(&format!("{test}-home"));
        Scratch {
            base,
            namespace: Namespace::new(&format!("agent-test-{test}-{}", std::process::id())),
            keep: Vec::new(),
        }
    }

    /// A namespace that outlives the process, so volumes kept in it are found next time.
    pub fn fixed(test: &str) -> Scratch {
        Scratch {
            base: temp(&format!("{test}-home")),
            namespace: Namespace::new(&format!("agent-test-{test}")),
            keep: Vec::new(),
        }
    }

    pub fn policy(&self, text: &str) {
        fs::write(self.base.join("policy.toml"), text).unwrap();
    }

    pub fn paths(&self) -> Paths {
        self.paths_for("claude")
    }

    pub fn paths_for(&self, agent: &str) -> Paths {
        let mut paths = Paths::new(&self.base, agent);
        paths.namespace = self.namespace.clone();
        paths
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.namespace.remove_all(&Podman::default(), &self.keep);
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// A canonical scratch folder, removed with the test's scratch home only if inside it.
pub fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("agent-{}-{name}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

/// An empty Git checkout.
pub fn checkout(name: &str) -> PathBuf {
    let root = temp(name);
    // None of the developer's Git configuration, templates or hooks.
    let git = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", "-q"])
        .arg(&root)
        .status();
    assert!(git.unwrap().success());
    root
}

pub fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}
