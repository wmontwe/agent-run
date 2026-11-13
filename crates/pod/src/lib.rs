//! A project's pod: one internal network, one pod with
//! one user namespace for all of the project's containers, one state volume, and the mount
//! plan the pod started with.
//!
//! The plan is stored in the launcher's state folder on the host (never mounted); its hash
//! and the hash of the policy's workspace rules are labels on the pod. A later session
//! reuses that plan, so every container in the pod has the same protections, and is
//! refused when the rules changed since: `agent restart <project>` recreates the pod.

use mountplan::Plan;
use podman::{Namespace, Object, Podman, PodmanError};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

const PLAN_HASH: &str = "agent-run.plan-hash";
const RULES_HASH: &str = "agent-run.rules-hash";
const CHECKOUT: &str = "agent-run.checkout";

/// A project, identified by its canonical checkout path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// `<folder name>-<8 hex digits of the path's hash>`: readable, and unique per path.
    pub id: String,
    pub checkout: PathBuf,
    pub namespace: Namespace,
}

impl Project {
    pub fn new(namespace: &Namespace, checkout: &Path) -> Project {
        let name: String = checkout
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let digest = Sha256::digest(checkout.as_os_str().as_encoded_bytes());
        let hash: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
        let name = name.trim_matches('-');
        let id = format!("{}-{hash}", if name.is_empty() { "project" } else { name });
        Project {
            id,
            checkout: checkout.to_path_buf(),
            namespace: namespace.clone(),
        }
    }

    fn name(&self, suffix: &str) -> String {
        self.namespace.name(&format!("{}{suffix}", self.id))
    }

    pub fn pod(&self) -> String {
        self.name("")
    }

    pub fn network(&self) -> String {
        self.name("")
    }

    /// Build state and private folders.
    pub fn state_volume(&self) -> String {
        self.name("-state")
    }

    /// The project's build container, which lives with the pod.
    pub fn build_container(&self) -> String {
        self.name("-build")
    }

    /// The project's Gradle home: caches, daemons' state, wrapper distributions. Kept
    /// across pods.
    pub fn gradle_volume(&self) -> String {
        self.name("-gradle")
    }

    /// The project's Android SDK. Kept across pods.
    pub fn android_volume(&self) -> String {
        self.name("-android")
    }

    /// The project's engram service, outside the pod.
    pub fn engram_container(&self) -> String {
        self.name("-engram")
    }

    /// The socket the project's agents reach its engram service on.
    pub fn engram_volume(&self) -> String {
        self.name("-engram")
    }

    /// A session's container in the pod, by its display number (unique among running sessions).
    pub fn session_container(&self, agent: &str, session: u32) -> String {
        self.name(&format!("-{agent}-{session}"))
    }

    /// Creates a volume of the project with the namespace's label, unless it exists.
    pub fn volume(&self, podman: &Podman, name: &str) -> Result<bool, PodError> {
        if podman.exists(Object::Volume, name)? {
            return Ok(false);
        }
        podman.run(&["volume", "create", "--label", &self.namespace.label(), name])?;
        Ok(true)
    }
}

/// The checkout a command run in `folder` is about: the checkout of the namespace's running
/// pod that contains it (the deepest, if several do), or the folder itself.
pub fn checkout_for(
    podman: &Podman,
    namespace: &Namespace,
    folder: &Path,
) -> Result<PathBuf, PodError> {
    let mut best: Option<PathBuf> = None;
    for checkout in checkouts(podman, namespace)? {
        if folder.starts_with(&checkout)
            && best
                .as_ref()
                .is_none_or(|b| checkout.components().count() > b.components().count())
        {
            best = Some(checkout);
        }
    }
    Ok(best.unwrap_or_else(|| folder.to_path_buf()))
}

/// The checkouts of the namespace's pods.
pub fn checkouts(podman: &Podman, namespace: &Namespace) -> Result<Vec<PathBuf>, PodError> {
    let filter = format!("label={}", namespace.label());
    let listing = podman.run(&["pod", "ls", "--filter", &filter, "--format", "{{.Name}}"])?;
    let mut checkouts = Vec::new();
    for pod in listing.lines().filter(|l| !l.is_empty()) {
        if let Some(checkout) = podman.labels(Object::Pod, pod)?.get(CHECKOUT) {
            checkouts.push(PathBuf::from(checkout));
        }
    }
    Ok(checkouts)
}

/// The state volume's subpath for the build state (build folders, project cache).
pub const BUILD_STATE: &str = "build";

#[derive(Debug)]
pub enum PodError {
    Podman(PodmanError),
    Io(String),
    /// The policy's workspace rules changed since the pod started.
    RulesChanged,
    /// The pod exists, but its stored plan is missing or doesn't match it.
    PlanLost,
}

impl fmt::Display for PodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PodError::Podman(e) => e.fmt(f),
            PodError::Io(e) => f.write_str(e),
            PodError::RulesChanged => f.write_str(
                "the policy's workspace rules changed since this project's pod started; run `agent restart` in the \
                 project (it stops the project's sessions)",
            ),
            PodError::PlanLost => f.write_str("the pod's stored mount plan is missing or altered; run `agent restart` in the project"),
        }
    }
}

impl std::error::Error for PodError {}

impl From<PodmanError> for PodError {
    fn from(e: PodmanError) -> Self {
        PodError::Podman(e)
    }
}

fn io(context: &Path, e: std::io::Error) -> PodError {
    PodError::Io(format!("{}: {e}", context.display()))
}

/// What `ensure` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Ready {
    Created,
    /// The pod was running already; `unprotected` lists folders the current rules would
    /// protect that its plan doesn't cover, until the pod is recreated.
    Reused {
        unprotected: Vec<String>,
    },
}

/// Settings for creating a pod.
pub struct Setup<'a> {
    /// Where plans are stored, on the host.
    pub state_dir: &'a Path,
    /// An image with `mkdir`, run once to create the plan's volume subpaths.
    pub helper_image: &'a str,
}

/// Makes sure the project's pod runs with a plan, and returns the plan every new container
/// must be verified against: the stored one when the pod exists, else `fresh`.
pub fn ensure(
    podman: &Podman,
    project: &Project,
    fresh: Plan,
    rules_hash: &str,
    setup: &Setup,
) -> Result<(Plan, Ready), PodError> {
    let plan_file = setup.state_dir.join(format!("{}.plan", project.id));
    if podman.exists(Object::Pod, &project.pod())? {
        let labels = podman.labels(Object::Pod, &project.pod())?;
        if labels.get(RULES_HASH).map(String::as_str) != Some(rules_hash) {
            return Err(PodError::RulesChanged);
        }
        let stored = fs::read_to_string(&plan_file).map_err(|_| PodError::PlanLost)?;
        let stored = Plan::from_text(&stored).map_err(|_| PodError::PlanLost)?;
        if labels.get(PLAN_HASH) != Some(&stored.hash()) {
            return Err(PodError::PlanLost);
        }
        let unprotected = fresh
            .mounts
            .iter()
            .filter(|m| {
                !stored
                    .mounts
                    .iter()
                    .any(|s| s.relative == m.relative && s.kind == m.kind)
            })
            .map(|m| m.relative.clone())
            .collect();
        return Ok((stored, Ready::Reused { unprotected }));
    }

    if !podman.exists(Object::Network, &project.network())? {
        // Internal: the pod's only way out is the egress proxy.
        podman.run(&[
            "network",
            "create",
            "--internal",
            "--label",
            &project.namespace.label(),
            &project.network(),
        ])?;
    }
    project.volume(podman, &project.state_volume())?;
    let mut subpaths = fresh.subpaths();
    subpaths.push(BUILD_STATE);
    {
        // Podman mounts a volume subpath only if it exists.
        let mut args: Vec<String> = [
            "run",
            "--rm",
            "--network",
            "none",
            "--userns",
            "keep-id:uid=1000,gid=1000",
        ]
        .map(String::from)
        .into();
        args.extend([
            "-v".into(),
            format!("{}:/state", project.state_volume()),
            "--workdir".into(),
            "/state".into(),
        ]);
        args.extend([
            "--entrypoint".into(),
            "mkdir".into(),
            setup.helper_image.to_owned(),
            "-p".into(),
        ]);
        args.extend(subpaths.iter().map(|s| (*s).to_owned()));
        podman.run(&args)?;
    }
    fs::create_dir_all(setup.state_dir).map_err(|e| io(setup.state_dir, e))?;
    fs::write(&plan_file, fresh.to_text()).map_err(|e| io(&plan_file, e))?;
    podman.run(&[
        "pod",
        "create",
        "--name",
        &project.pod(),
        "--network",
        &project.network(),
        // One user namespace for the whole pod: the same mapping for all.
        "--userns",
        "keep-id:uid=1000,gid=1000",
        "--label",
        &format!("{PLAN_HASH}={}", fresh.hash()),
        "--label",
        &format!("{RULES_HASH}={rules_hash}"),
        "--label",
        &format!("{CHECKOUT}={}", project.checkout.display()),
        "--label",
        &project.namespace.label(),
    ])?;
    Ok((fresh, Ready::Created))
}

/// Removes the pod once no session is left in it, running or stopped, so the next session plans
/// anew. A session a killed launcher left stopped keeps it until `agent restart`.
/// Returns whether it was removed. Volumes are kept.
pub fn remove_if_idle(podman: &Podman, project: &Project, setup: &Setup) -> Result<bool, PodError> {
    if !podman.exists(Object::Pod, &project.pod())? {
        return Ok(false);
    }
    // Stopped sessions count too: their launcher still collects their status and cleans up.
    let sessions = podman.run(&[
        "ps",
        "--all",
        "--filter",
        &format!("pod={}", project.pod()),
        "--format",
        "{{.Names}}",
    ])?;
    // The pod's infra container and its build container aren't sessions.
    let build = project.build_container();
    if sessions
        .lines()
        .any(|name| !name.ends_with("-infra") && name != build)
    {
        return Ok(false);
    }
    remove(podman, project, setup)?;
    Ok(true)
}

/// Removes the pod and its stored plan, stopping its containers; `agent restart`.
pub fn remove(podman: &Podman, project: &Project, setup: &Setup) -> Result<(), PodError> {
    if podman.exists(Object::Pod, &project.pod())? {
        podman.run(&["pod", "rm", "--force", "--time", "5", &project.pod()])?;
    }
    let plan_file = setup.state_dir.join(format!("{}.plan", project.id));
    match fs::remove_file(&plan_file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(&plan_file, e)),
        _ => Ok(()),
    }
}

/// Removes the project's network once its pod is gone. Fails while a container (the proxy)
/// is still connected: disconnect it first.
pub fn remove_network(podman: &Podman, project: &Project) -> Result<(), PodError> {
    if podman.exists(Object::Network, &project.network())? {
        podman.run(&["network", "rm", &project.network()])?;
    }
    Ok(())
}

/// Held while a pod is set up and a container starts. Rootless Podman fails containers with
/// `keep-id` that start at the same moment (`crun: fchownat ... Invalid argument`), and two
/// sessions mustn't create the same pod twice.
pub struct StartLock(#[allow(dead_code)] File);

pub fn lock(state_dir: &Path) -> Result<StartLock, PodError> {
    lock_unless(state_dir, || false)
}

/// Like [`lock`], but gives up waiting once `cancelled` returns true.
pub fn lock_unless(state_dir: &Path, cancelled: impl Fn() -> bool) -> Result<StartLock, PodError> {
    fs::create_dir_all(state_dir).map_err(|e| io(state_dir, e))?;
    let path = state_dir.join("start.lock");
    let file = File::create(&path).map_err(|e| io(&path, e))?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(StartLock(file)),
            Err(std::fs::TryLockError::WouldBlock) if cancelled() => {
                return Err(PodError::Io(
                    "stopped while waiting for another session's start".into(),
                ));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(100))
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(io(&path, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_projects_readably_and_uniquely() {
        let ns = Namespace::default();
        let a = Project::new(&ns, Path::new("/Users/me/Projects/app"));
        let b = Project::new(&ns, Path::new("/Users/me/Other/app"));
        assert!(a.id.starts_with("app-") && a.id.len() == "app-".len() + 8);
        assert_ne!(a.id, b.id);
        assert_eq!(a.pod(), format!("agent-run-{}", a.id));
        assert_eq!(a.state_volume(), format!("agent-run-{}-state", a.id));
        assert!(
            Project::new(&ns, Path::new("/x/My Project_1"))
                .id
                .starts_with("my-project-1-")
        );
        assert!(Project::new(&ns, Path::new("/")).id.starts_with("project-"));
        let test = Project::new(&Namespace::new("t"), Path::new("/x/app"));
        assert_eq!(
            (test.pod(), test.state_volume()),
            (format!("t-{}", test.id), format!("t-{}-state", test.id))
        );
    }

    #[test]
    fn the_start_lock_excludes_a_second_holder_until_released() {
        let dir = std::env::temp_dir().join(format!("pod-lock-test-{}", std::process::id()));
        let held = lock(&dir).unwrap();
        let other = File::open(dir.join("start.lock")).unwrap();
        assert!(matches!(
            other.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(held);
        assert!(other.try_lock().is_ok());
    }
}
