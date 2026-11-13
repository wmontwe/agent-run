//! One agent session: the steps in order, each done by its crate.

use crate::services::{self, Services};
use crate::{Paths, Progress, home_dir, images, repository};
use mountplan::Plan;
use pod::{Project, Ready, Setup};
use podman::Podman;
use policy::Policy;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// A started session: its container runs the agent's command, released by the gate.
pub struct Session {
    pub name: String,
    /// The project's engram setup, if the policy links it, and the thread committing its saves.
    engram: Option<(engram::Engram, Saver)>,
    /// Its display (and browser) in the build container.
    pub display: u32,
    pub project: Project,
    pub plan: Plan,
    slot: Slot,
    policy: Policy,
    paths: Paths,
    image: String,
    podman: Podman,
}

/// What a session owns beside the pod it shares: its container, whose name (by display number)
/// reserves the slot until it's removed, the display and browser, its proxy identity, its
/// files and secret. Released under the start lock, container last, so a start never picks the
/// slot while parts of it remain.
struct Slot {
    name: String,
    display: u32,
    session_dir: PathBuf,
    /// The session's proxy user, once it has one: what it reaches is its agent's.
    proxy_user: Option<String>,
}

impl Slot {
    fn release(&self, podman: &Podman, paths: &Paths, project: &Project) {
        let _ = vision::stop(podman, project, self.display);
        if let Some(user) = &self.proxy_user {
            Services::forget(paths, user);
        }
        // The session's root-owned file, if its agent needed one; nothing to do otherwise.
        let _ = podman.run(&["secret", "rm", &self.name]);
        let _ = fs::remove_dir_all(&self.session_dir);
        let _ = podman.run(&["rm", "--force", "--time", "5", &self.name]);
    }
}

/// Starts `agent` (or `command` instead of its CLI) in the checkout's pod: the checks and the
/// plan, the pod, the shared services and the build container, the session's display and
/// browser, then the session's container behind the gate, released only once its mounts match
/// the plan.
pub fn start(
    agent: &str,
    checkout: &Path,
    command: Option<&[String]>,
    paths: Paths,
) -> Result<Session, String> {
    let session = prepare(agent, checkout, command, paths)?;
    if let Err(e) = mountplan::release(&session.podman, &session.name) {
        session.end();
        return Err(e);
    }
    Ok(session)
}

/// Like [`start`], but leaves the command held by the gate: [`Session::run`] attaches the
/// terminal first, then releases it, so nothing it prints or returns is missed.
pub fn prepare(
    agent: &str,
    checkout: &Path,
    command: Option<&[String]>,
    paths: Paths,
) -> Result<Session, String> {
    let cli = agents::Agent::parse(agent).ok_or_else(|| format!("unknown agent {agent}"))?;
    let mut progress = Progress::start();
    let podman = Podman::default();
    let policy = progress.step("policy", || paths.load_policy())?;
    let project = Project::new(&paths.namespace, checkout);
    let home = home_dir()?;

    progress.step("rootless Podman", || match podman.is_rootless() {
        Ok(true) => Ok(()),
        Ok(false) => Err(
            "Podman runs rootful; agent-run needs rootless Podman (--userns keep-id)".to_owned(),
        ),
        Err(e) => Err(e.to_string()),
    })?;
    let fresh = progress.step("checkout and plan", || {
        workspace::check_location(checkout).map_err(|e| e.to_string())?;
        let rules = &policy.workspace;
        let rules = workspace::Rules {
            mask: &rules.mask,
            read_only: &rules.read_only,
            private: &rules.private,
        };
        let inspection = workspace::inspect(checkout, &rules).map_err(|e| e.to_string())?;
        workspace::check_ide(checkout).map_err(|e| e.to_string())?;
        workspace::check_git(checkout, &inspection, &home).map_err(|e| e.to_string())?;
        if let Some(engram) = policy.for_project(checkout).engram {
            workspace::check_engram(checkout, engram).map_err(|e| e.to_string())?;
        }
        mountplan::plan(checkout, &inspection, &project.state_volume()).map_err(|e| e.to_string())
    })?;
    let sdk_dir = workspace::sdk_dir(checkout, &home).map_err(|e| e.to_string())?;
    // What the start acquires stops once the launcher is told to; cleanup doesn't.
    let cleanup = podman;
    let podman = cleanup.cancelled_by(crate::signals::stopped);
    let engram = policy
        .for_project(checkout)
        .engram
        .map(engram::Engram::resolve)
        .transpose()?;
    let save_requests = paths.state.join("engram").join(&project.id);
    let rules_hash = mountplan::rules_hash(
        &policy.workspace.mask,
        &policy.workspace.read_only,
        &policy.workspace.private,
    );
    progress.step("images", || images::ensure(&podman, &repository(), agent))?;

    let image = images::agent(agent);
    let (plan, slot) = {
        // Pod setup and container starts run one at a time (see pod::lock).
        let _lock =
            pod::lock_unless(&paths.state, crate::signals::stopped).map_err(|e| e.to_string())?;
        // From the pod on: if anything fails, the pod goes again, unless another session runs in
        // it (a start that fails must not leave a pod behind).
        let rest = (|| -> Result<_, String> {
            let plan = progress.step("pod", || {
                let setup = Setup { state_dir: &paths.state, helper_image: &image };
                let (plan, ready) = pod::ensure(&podman, &project, fresh, &rules_hash, &setup).map_err(|e| e.to_string())?;
                if let Ready::Reused { unprotected } = &ready
                    && !unprotected.is_empty()
                {
                    eprintln!("agent-run: not protected until `agent restart` (the pod started before they existed): {}", unprotected.join(", "));
                }
                Ok(plan)
            })?;
            progress.step("services", || {
                Services::new(&podman, &paths).start(&policy, &paths, &project.network())
            })?;
            progress.step("build container", || {
                let settings = builds::Settings {
                    image: images::BUILD,
                    memory: policy.for_project(checkout).build_memory,
                    cpus: policy.build_cpus,
                    max_concurrent: policy.max_concurrent_builds,
                    mirror_remotes: policy
                        .mirror_remotes
                        .iter()
                        .map(|(id, url)| format!("{id}={url}"))
                        .collect(),
                    mirror_url: mirror::URL,
                    sdk_dir: sdk_dir.as_deref(),
                    host_sdk: sdk_dir
                        .clone()
                        .or_else(|| std::env::var_os("ANDROID_HOME").map(PathBuf::from))
                        .or(Some(home.join("Library/Android/sdk"))),
                    licenses_copied: &paths.state.join(format!("{}.licenses", project.id)),
                };
                builds::ensure(&podman, &project, &plan, &settings)
            })?;
            if let Some(engram) = &engram {
                progress.step("engram", || {
                    engram.ensure(&podman, &project, images::BUILD, &save_requests)
                })?;
            }
            // The display number is unique among the pod's sessions: it names the session.
            let display = vision::free_display(&podman, &project)?;
            let name = project.session_container(agent, display);
            let mut slot = Slot {
                session_dir: paths.state.join("sessions").join(&name),
                name: name.clone(),
                display,
                proxy_user: None,
            };
            let started = (|| -> Result<(), String> {
                let (proxy_user, proxy_url) = Services::identity(&paths, agent)?;
                slot.proxy_user = Some(proxy_user.clone());
                progress.step("display and browser", || {
                    vision::start(&podman, &project, display, &name)
                })?;
                progress.step("container", || {
                    let host = sessions::Host {
                        agent_home: paths.agent_home.clone(),
                        instructions: paths.instructions.clone(),
                        knowledge: paths.knowledge.clone(),
                    };
                    let mut extra = vision::container_args(display);
                    let mut servers = vec![agents::Server::Http {
                        name: "playwright".into(),
                        url: vision::browser_url(display),
                    }];
                    if let Some(engram) = &engram {
                        extra.extend(engram.agent_args(&project));
                        let (command, args) = engram::Engram::bridge();
                        servers.push(agents::Server::Stdio {
                            name: "engram".into(),
                            command,
                            args,
                        });
                    }
                    let launch = cli.launch(&servers, &paths.agent_home)?;
                    // A file that must belong to root: a Podman secret, removed with the session.
                    if let Some((content, target)) = &launch.root_file {
                        secret(&podman, &name, content)?;
                        extra.extend([
                            "--secret".into(),
                            format!("{name},type=mount,target={target},uid=0,gid=0,mode=0444"),
                        ]);
                    }
                    let instructions_extra = (engram.is_some() && !cli.reads_claude_local())
                        .then_some(engram::Engram::INSTRUCTIONS);
                    extra.extend([
                        "--label".into(),
                        format!("{}={agent}", services::AGENT_LABEL),
                        "--label".into(),
                        format!("{}={proxy_user}", services::PROXY_USER_LABEL),
                    ]);
                    let settings = sessions::Settings {
                        name: &name,
                        image: &image,
                        memory: policy.agent_memory,
                        cpus: policy.agent_cpus,
                        relay_url: relay::URL,
                        proxy_url: &proxy_url,
                        extra,
                        instructions_at: cli.instructions(),
                        instructions_extra,
                        env: &launch.env,
                        files: &launch.files,
                        session_dir: &slot.session_dir,
                    };
                    let command = command.map(<[String]>::to_vec).unwrap_or(launch.command);
                    let args = sessions::run_args(&project, &plan, &host, &settings, &command)?;
                    podman.run(&args).map(|_| ()).map_err(|e| e.to_string())?;
                    // The session's own rules, before the gate releases it.
                    Services::new(&podman, &paths).rules(&policy, &paths)
                })
            })();
            if let Err(e) = started {
                slot.release(&cleanup, &paths, &project);
                return Err(e);
            }
            Ok((plan, slot))
        })();
        match rest {
            Ok(acquired) => acquired,
            Err(e) => {
                let setup = Setup {
                    state_dir: &paths.state,
                    helper_image: &image,
                };
                if let Ok(true) = pod::remove_if_idle(&cleanup, &project, &setup) {
                    let _ = forget_pod(&cleanup, &paths, &project, Some(&policy));
                }
                return Err(e);
            }
        }
    };
    let engram = engram.map(|e| {
        let saver = Saver::start(e.clone(), save_requests.clone());
        (e, saver)
    });
    let session = Session {
        name: slot.name.clone(),
        display: slot.display,
        slot,
        engram,
        project,
        plan,
        policy,
        paths,
        image,
        podman: cleanup,
    };
    if let Err(e) = progress.step("mount verification", || {
        mountplan::check_mounts(&session.podman, &session.name, &session.plan)
    }) {
        session.end();
        return Err(e);
    }
    eprintln!("agent-run: {agent} ready after {} ms", progress.total());
    Ok(session)
}

/// What the gate prints while it holds a session's command (`agent-run-gate attach`).
const ANNOUNCE: &[u8] = b"\x1b_agent-run-gate\x1b\\";

impl Session {
    /// For a session from [`prepare`]: attaches the terminal, releases the command once the
    /// gate's announcement arrives through the attachment (then nothing the command prints is
    /// missed), waits for it and ends the session. Returns the command's exit status (`podman
    /// attach` doesn't). No detach keys: Claude uses Ctrl-P.
    pub fn run(self) -> Result<i32, String> {
        let status = self.attached();
        self.end();
        status
    }

    fn attached(&self) -> Result<i32, String> {
        let mut attach = Command::new("podman")
            .args(["attach", "--detach-keys", "", &self.name])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("podman attach: {e}"))?;
        let (ready, announced) = mpsc::channel();
        let released = Arc::new(AtomicBool::new(false));
        let output = attach.stdout.take().expect("piped");
        let forward = {
            let released = released.clone();
            thread::spawn(move || forward(output, ready, &released))
        };
        let result = (|| {
            // Until the terminal is attached.
            let deadline = Instant::now() + Duration::from_secs(60);
            while announced.try_recv().is_err() {
                if crate::signals::stopped() {
                    return Ok(143);
                }
                if let Some(status) = attach.try_wait().map_err(|e| e.to_string())? {
                    return Err(format!(
                        "podman attach ended before the terminal was connected ({status}); ending the session"
                    ));
                }
                if Instant::now() > deadline {
                    return Err("the terminal wasn't connected (podman attach) within 60 s; ending the session".into());
                }
                thread::sleep(Duration::from_millis(20));
            }
            released.store(true, Ordering::SeqCst);
            mountplan::release(&self.podman, &self.name)?;
            // Until the command ends, or the launcher is told to stop.
            loop {
                if crate::signals::stopped() {
                    return Ok(143);
                }
                if attach.try_wait().map_err(|e| e.to_string())?.is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            // The attachment ends with the container; if it still runs, the terminal was lost.
            for _ in 0..50 {
                let state = self
                    .podman
                    .run(&[
                        "inspect",
                        "--format",
                        "{{.State.Status}} {{.State.ExitCode}}",
                        &self.name,
                    ])
                    .map_err(|e| e.to_string())?;
                if let Some(code) = state.trim().strip_prefix("exited ") {
                    return code
                        .parse::<i32>()
                        .map_err(|_| format!("podman inspect: {state}"));
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err("the terminal's connection (podman attach) ended while the agent still runs; ending the session".into())
        })();
        let _ = attach.kill();
        let _ = attach.wait();
        let _ = forward.join();
        result
    }

    /// Stops the container; with the project's last session, its pod goes too. Failures only
    /// warn: the session is over.
    pub fn end(self) {
        // Stopped, not removed: the container reserves the slot until it's released.
        let _ = self.podman.run(&["stop", "--time", "5", &self.name]);
        // What the session saved: the last requests, then what the saver did meanwhile. Not
        // under the start lock: a save can wait for a passphrase, and starts mustn't.
        if let Some((engram, saver)) = self.engram {
            let mut report = saver.stop();
            match engram.save(&self.paths.state.join("engram").join(&self.project.id)) {
                Ok(Some(summary)) => report.push(format!(
                    "committed {}'s engram context: {summary}",
                    engram.project
                )),
                Ok(None) => {}
                Err(e) => report.push(format!("warning: {e}")),
            }
            report.dedup();
            report
                .iter()
                .for_each(|line| eprintln!("agent-run: {line}"));
        }
        // A session starting now must not take the slot, or set up the pod, while they go.
        let _lock = pod::lock(&self.paths.state);
        self.slot.release(&self.podman, &self.paths, &self.project);
        let setup = Setup {
            state_dir: &self.paths.state,
            helper_image: &self.image,
        };
        match pod::remove_if_idle(&self.podman, &self.project, &setup) {
            Ok(true) => {
                if let Err(e) =
                    forget_pod(&self.podman, &self.paths, &self.project, Some(&self.policy))
                {
                    eprintln!("agent-run: warning: {e}");
                }
            }
            // The session's identity is gone: its rules go too.
            Ok(false) => {
                if let Err(e) =
                    Services::new(&self.podman, &self.paths).rules(&self.policy, &self.paths)
                {
                    eprintln!("agent-run: warning: {e}");
                }
            }
            Err(e) => eprintln!("agent-run: warning: {e}"),
        }
    }
}

/// Copies the attachment's output to the terminal, without the gate's announcements before the
/// command's first output; tells `ready` at each one.
fn forward(mut from: impl Read, ready: mpsc::Sender<()>, released: &AtomicBool) {
    let mut out = std::io::stdout();
    let mut pending: Vec<u8> = Vec::new();
    let mut gate = true;
    let mut buffer = [0u8; 8192];
    while let Ok(n) = from.read(&mut buffer) {
        if n == 0 {
            break;
        }
        if !gate {
            let _ = out.write_all(&buffer[..n]).and_then(|()| out.flush());
            continue;
        }
        pending.extend_from_slice(&buffer[..n]);
        loop {
            if pending.starts_with(ANNOUNCE) {
                pending.drain(..ANNOUNCE.len());
                let _ = ready.send(());
            } else if ANNOUNCE.starts_with(&pending) {
                break; // Empty, or the start of an announcement: wait for the rest.
            } else if released.load(Ordering::SeqCst) {
                // The command's output: the gate announces nothing after it.
                gate = false;
                let _ = out.write_all(&pending);
                pending.clear();
                break;
            } else {
                // Not an announcement (an echo of a key typed early): up to the next escape.
                let next = pending[1..]
                    .iter()
                    .position(|&b| b == 0x1b)
                    .map_or(pending.len(), |i| i + 1);
                let _ = out.write_all(&pending[..next]);
                pending.drain(..next);
            }
        }
        let _ = out.flush();
    }
}

/// Commits a session's engram saves within seconds, on the host, signed by the user, while the
/// agent runs. It prints nothing: the agent's interface is on the terminal. What it did is
/// reported when the session ends; a failed save stays recorded and is retried.
struct Saver {
    stop: Arc<AtomicBool>,
    thread: thread::JoinHandle<Vec<String>>,
}

impl Saver {
    fn start(engram: engram::Engram, requests: PathBuf) -> Saver {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            let mut report = Vec::new();
            while !flag.load(Ordering::SeqCst) {
                match engram.save(&requests) {
                    Ok(Some(summary)) => report.push(format!(
                        "committed {}'s engram context: {summary}",
                        engram.project
                    )),
                    Ok(None) => {}
                    Err(e) => report.push(format!("warning: {e}")),
                }
                for _ in 0..30 {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
            report
        });
        Saver { stop, thread }
    }

    fn stop(self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.join().unwrap_or_default()
    }
}

/// Stores a session's root-owned file as a Podman secret of the session's name.
fn secret(podman: &Podman, name: &str, content: &str) -> Result<(), String> {
    let _ = podman.run(&["secret", "rm", name]);
    let mut child = Command::new("podman")
        .args(["secret", "create", name, "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("podman secret create: {e}"))?;
    std::io::Write::write_all(&mut child.stdin.take().expect("piped"), content.as_bytes())
        .map_err(|e| e.to_string())?;
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        _ => Err(format!("podman secret create {name} failed")),
    }
}

/// After a pod is removed: its session approvals, its rules in the proxy, the services'
/// connections to its network, the network.
fn forget_pod(
    podman: &Podman,
    paths: &Paths,
    project: &Project,
    policy: Option<&Policy>,
) -> Result<(), String> {
    approvals::Session::clear_project(&paths.state, &project.id).map_err(|e| e.to_string())?;
    engram::Engram::remove(podman, project);
    let services = Services::new(podman, paths);
    if let Some(policy) = policy {
        services.rules(policy, paths)?;
    }
    services.leave(&project.network())?;
    pod::remove_network(podman, project).map_err(|e| e.to_string())
}

/// `agent shutdown`: stops everything agent-run runs (pods, build containers, engram services,
/// the shared services); volumes stay, so builds, caches and the mirror's data are kept. With
/// sessions running, refused unless `force`, which ends them.
pub fn shutdown(paths: &Paths, force: bool) -> Result<(), String> {
    let podman = Podman::default();
    let _lock = pod::lock(&paths.state).map_err(|e| e.to_string())?;
    let projects: Vec<Project> = pod::checkouts(&podman, &paths.namespace)
        .map_err(|e| e.to_string())?
        .iter()
        .map(|c| Project::new(&paths.namespace, c))
        .collect();
    let mut active = Vec::new();
    for project in &projects {
        if !services::session_identities(&podman, project)?.is_empty() {
            active.push(project.checkout.display().to_string());
        }
    }
    if !active.is_empty() && !force {
        return Err(format!(
            "sessions run in {}; end them first, or `agent shutdown --force` ends them too",
            active.join(", ")
        ));
    }
    paths
        .namespace
        .stop_all(&podman)
        .map_err(|e| e.to_string())?;
    // What the pods' sessions had: approvals and proxy credentials.
    for project in &projects {
        approvals::Session::clear_project(&paths.state, &project.id).map_err(|e| e.to_string())?;
    }
    Services::forget_all(paths);
    eprintln!(
        "agent-run: stopped; volumes (build state, Gradle homes, SDKs, the mirror's data) are kept"
    );
    Ok(())
}

pub fn restart(checkout: &Path) -> Result<(), String> {
    let paths = Paths::from_env("claude")?;
    let podman = Podman::default();
    let project = Project::new(&paths.namespace, checkout);
    let _lock = pod::lock(&paths.state).map_err(|e| e.to_string())?;
    pod::remove(
        &podman,
        &project,
        &Setup {
            state_dir: &paths.state,
            helper_image: "",
        },
    )
    .map_err(|e| e.to_string())?;
    forget_pod(&podman, &paths, &project, paths.load_policy().ok().as_ref())?;
    eprintln!(
        "agent-run: removed {}'s pod; the next session plans it anew",
        checkout.display()
    );
    Ok(())
}
