//! The build container's side: one server per project, at most `max_concurrent` builds at
//! once, each with the container's fixed build settings plus request arguments.

use crate::{Frame, Request, read_request, wrapper, write_frame};
use std::io::{BufReader, Read};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// The build container's fixed layout and the project's settings, from the environment the
/// launcher gives the container.
#[derive(Debug, Clone)]
pub struct Config {
    /// The checkout, at its host path.
    pub workspace: PathBuf,
    pub gradle_home: PathBuf,
    /// The build state volume: build folders, project cache, Kotlin's state.
    pub build_state: PathBuf,
    /// Base URL of the mirror, such as `http://mirror:8080`.
    pub mirror: String,
    /// The image's JDKs, for `org.gradle.java.installations.paths`.
    pub jdks: Vec<PathBuf>,
    /// Folder of the init scripts.
    pub init_scripts: PathBuf,
    pub max_concurrent: usize,
    /// Runs instead of the wrapper (tests: a stand-in build).
    pub command: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        let var = |key: &str| std::env::var(key).map_err(|_| format!("{key} is not set"));
        Ok(Config {
            workspace: PathBuf::from(var("AGENT_RUN_WORKSPACE")?),
            gradle_home: PathBuf::from(var("GRADLE_USER_HOME")?),
            build_state: PathBuf::from(var("AGENT_RUN_BUILD_STATE")?),
            mirror: var("AGENT_RUN_MIRROR")?,
            jdks: var("AGENT_RUN_JDKS")?
                .split(',')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect(),
            init_scripts: PathBuf::from(var("AGENT_RUN_INIT_SCRIPTS")?),
            max_concurrent: var("AGENT_RUN_MAX_CONCURRENT")?
                .parse()
                .map_err(|_| "AGENT_RUN_MAX_CONCURRENT: not a number".to_owned())?,
            command: std::env::var_os("AGENT_RUN_BUILD_COMMAND").map(PathBuf::from),
        })
    }

    /// The options every build gets before the agent's own.
    pub fn options(&self) -> Vec<String> {
        let state = |rel: &str| self.build_state.join(rel).display().to_string();
        let jdks: Vec<String> = self.jdks.iter().map(|j| j.display().to_string()).collect();
        vec![
            "--no-problems-report".into(),
            "--init-script".into(),
            self.init_scripts
                .join("redirect.init.gradle.kts")
                .display()
                .to_string(),
            "--init-script".into(),
            self.init_scripts
                .join("mirror.init.gradle.kts")
                .display()
                .to_string(),
            "--project-cache-dir".into(),
            state("project-cache"),
            format!("-Pkotlin.project.persistent.dir={}", state("kotlin")),
            format!("-Porg.gradle.java.installations.paths={}", jdks.join(",")),
        ]
    }

    /// The environment every build gets, besides the container's.
    pub fn environment(&self) -> Vec<(String, String)> {
        vec![
            (
                "ANDROID_USER_HOME".into(),
                self.build_state.join("android-user").display().to_string(),
            ),
            (
                "SDK_TEST_BASE_URL".into(),
                format!("{}/android-sdk/", self.mirror.trim_end_matches('/')),
            ),
        ]
    }
}

/// At most `max` builds at once; the others wait.
pub struct Queue {
    running: Mutex<usize>,
    freed: Condvar,
    max: usize,
}

impl Queue {
    pub fn new(max: usize) -> Queue {
        Queue {
            running: Mutex::new(0),
            freed: Condvar::new(),
            max: max.max(1),
        }
    }

    /// Takes a slot; `waiting` is called once if the build has to wait. `None` if `cancelled`
    /// became true while waiting.
    pub fn enter(&self, cancelled: &AtomicBool, waiting: impl FnOnce(usize)) -> Option<Slot<'_>> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let mut waiting = Some(waiting);
        while *running >= self.max {
            if let Some(tell) = waiting.take() {
                tell(*running);
            }
            if cancelled.load(Ordering::SeqCst) {
                return None;
            }
            running = self
                .freed
                .wait_timeout(running, Duration::from_millis(200))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *running += 1;
        Some(Slot(self))
    }
}

pub struct Slot<'a>(&'a Queue);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.0.running.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        self.0.freed.notify_one();
    }
}

/// The folder with the wrapper for a build started in `cwd`: the nearest one up to the
/// checkout's root, as running `gradlew` from a subfolder would find.
pub fn wrapper_root(workspace: &Path, cwd: &Path) -> Result<PathBuf, String> {
    let cwd = cwd
        .canonicalize()
        .map_err(|e| format!("{}: {e}", cwd.display()))?;
    let workspace = workspace
        .canonicalize()
        .map_err(|e| format!("{}: {e}", workspace.display()))?;
    if !cwd.starts_with(&workspace) {
        return Err(format!(
            "{} is outside the checkout {}",
            cwd.display(),
            workspace.display()
        ));
    }
    let mut folder = cwd.as_path();
    loop {
        if folder.join("gradlew").is_file() {
            return Ok(folder.to_path_buf());
        }
        if folder == workspace {
            return Err(format!(
                "no gradlew in {} or above it in the checkout",
                cwd.display()
            ));
        }
        folder = folder.parent().unwrap_or(&workspace);
    }
}

/// Serves one connection: reads the request, runs the build, streams it back.
pub fn handle(stream: TcpStream, config: &Config, queue: &Queue, installs: &Mutex<()>) {
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let writer = Arc::new(Mutex::new(stream));
    let send = |frame: Frame| {
        let mut out = writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = write_frame(&mut *out, &frame);
    };
    let say = |text: String| send(Frame::Err(format!("gradle-run: {text}\n").into_bytes()));
    let mut reader = BufReader::new(reader);
    let request = match read_request(&mut reader) {
        Ok(request) => request,
        Err(e) => return say(format!("bad request: {e}")),
    };
    // The client disconnecting (Ctrl+C) cancels: nothing else arrives after the request.
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = cancelled.clone();
        thread::spawn(move || {
            let mut byte = [0u8; 1];
            while matches!(reader.read(&mut byte), Ok(n) if n > 0) {}
            cancelled.store(true, Ordering::SeqCst);
        });
    }
    let code = match run(&request, config, queue, installs, &cancelled, &send, &say) {
        Ok(code) => code,
        Err(e) => {
            say(e);
            1
        }
    };
    send(Frame::Exit(code));
}

fn run(
    request: &Request,
    config: &Config,
    queue: &Queue,
    installs: &Mutex<()>,
    cancelled: &AtomicBool,
    send: &(dyn Fn(Frame) + Sync),
    say: &(dyn Fn(String) + Sync),
) -> Result<i32, String> {
    let cwd = PathBuf::from(&request.cwd);
    let root = wrapper_root(&config.workspace, &cwd)?;
    let Some(_slot) = queue.enter(cancelled, |running| {
        say(format!(
            "waiting: {running} build(s) of this project running (builds.max-concurrent = {})",
            queue.max
        ))
    }) else {
        return Ok(130);
    };
    if config.command.is_none() {
        // One install at a time; a cancelled request stops waiting, and stops installing.
        let _one = loop {
            match installs.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(e)) => break e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) if cancelled.load(Ordering::SeqCst) => {
                    return Ok(130);
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    thread::sleep(Duration::from_millis(50))
                }
            }
        };
        if let Some(message) =
            wrapper::install(&root, &config.gradle_home, &config.mirror, cancelled)?
        {
            say(message);
        }
    }
    if cancelled.load(Ordering::SeqCst) {
        return Ok(130);
    }
    let mut command = match &config.command {
        Some(stand_in) => Command::new(stand_in),
        None => {
            let mut c = Command::new(root.join("gradlew"));
            c.args(config.options());
            c
        }
    };
    command
        .args(&request.args)
        .current_dir(&cwd)
        .envs(config.environment())
        .stdin(Stdio::null());
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    match &request.display {
        Some(display) => command.env("DISPLAY", display),
        None => command.env_remove("DISPLAY"),
    };
    // Its own process group: what the build starts goes with it. Gradle's daemons leave the
    // group (they detach), so the project's shared daemons stay.
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| format!("starting the build: {e}"))?;
    let group = child.id() as i32;
    let (output, received) = mpsc::channel();
    for (pipe, is_out) in [
        (
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
            true,
        ),
        (
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
            false,
        ),
    ] {
        let Some(mut pipe) = pipe else { continue };
        let output = output.clone();
        thread::spawn(move || {
            let mut buffer = vec![0; 1 << 14];
            while let Ok(n) = pipe.read(&mut buffer) {
                if n == 0
                    || output
                        .send(if is_out {
                            Frame::Out(buffer[..n].to_vec())
                        } else {
                            Frame::Err(buffer[..n].to_vec())
                        })
                        .is_err()
                {
                    break;
                }
            }
        });
    }
    drop(output);
    let forward = |until: Duration| {
        while let Ok(frame) = received.recv_timeout(until) {
            send(frame);
        }
    };
    let status = loop {
        forward(Duration::from_millis(50));
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(e) => break Err(format!("waiting for the build: {e}")),
        }
        if cancelled.load(Ordering::SeqCst) {
            // Gradle's daemon cancels the build when its client goes.
            group::kill(group);
        }
    };
    // What the build left running goes too; its output is forwarded until the pipes close,
    // for a bounded time: a process that left the group mustn't hold the slot.
    group::kill(group);
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Ok(frame) = received.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        send(frame);
    }
    group::reap(group);
    Ok(status?.code().unwrap_or(130))
}

/// A build's process group.
mod group {
    unsafe extern "C" {
        #[link_name = "kill"]
        fn sys_kill(pid: i32, signal: i32) -> i32;
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }
    const SIGKILL: i32 = 9;
    const WNOHANG: i32 = 1;

    pub fn kill(group: i32) {
        // Safety: a plain system call on the build's own group (its leader's id).
        unsafe {
            sys_kill(-group, SIGKILL);
        }
    }

    /// Collects the group's processes that became the server's (it's the container's first
    /// process), so they don't stay as zombies. Nothing to collect elsewhere.
    pub fn reap(group: i32) {
        for _ in 0..20 {
            let mut status = 0;
            // Safety: waits only for the build's own group, without blocking.
            match unsafe { waitpid(-group, &mut status, WNOHANG) } {
                pid if pid > 0 => continue,
                0 => std::thread::sleep(std::time::Duration::from_millis(50)),
                _ => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn finds_the_nearest_wrapper_inside_the_checkout() {
        let root = std::env::temp_dir().join(format!("buildrun-root-{}", std::process::id()));
        fs::create_dir_all(root.join("app/src")).unwrap();
        fs::create_dir_all(root.join("tools/build-logic")).unwrap();
        fs::write(root.join("gradlew"), "").unwrap();
        fs::write(root.join("tools/build-logic/gradlew"), "").unwrap();
        let root = root.canonicalize().unwrap();
        assert_eq!(wrapper_root(&root, &root.join("app/src")).unwrap(), root);
        assert_eq!(
            wrapper_root(&root, &root.join("tools/build-logic")).unwrap(),
            root.join("tools/build-logic")
        );
        assert!(
            wrapper_root(&root.join("app"), &root.join("app/src"))
                .unwrap_err()
                .contains("no gradlew")
        );
        assert!(
            wrapper_root(&root.join("app"), &root)
                .unwrap_err()
                .contains("outside the checkout")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_queue_lets_max_builds_run_and_tells_the_others() {
        let queue = Arc::new(Queue::new(1));
        let told = Arc::new(AtomicUsize::new(0));
        let first = queue
            .enter(&AtomicBool::new(false), |_| {
                panic!("the first build doesn't wait")
            })
            .unwrap();
        let waiter = {
            let (queue, told) = (queue.clone(), told.clone());
            thread::spawn(move || {
                let slot = queue.enter(&AtomicBool::new(false), |running| {
                    assert_eq!(running, 1);
                    told.fetch_add(1, Ordering::SeqCst);
                });
                slot.is_some()
            })
        };
        thread::sleep(Duration::from_millis(300));
        assert_eq!(told.load(Ordering::SeqCst), 1);
        drop(first);
        assert!(waiter.join().unwrap());
        let cancelled = AtomicBool::new(true);
        let held = queue.enter(&AtomicBool::new(false), |_| {}).unwrap();
        assert!(
            queue.enter(&cancelled, |_| {}).is_none(),
            "a cancelled build leaves the queue"
        );
        drop(held);
    }

    #[test]
    fn every_build_gets_the_fixed_settings() {
        let config = Config {
            workspace: "/w".into(),
            gradle_home: "/gradle".into(),
            build_state: "/build-state".into(),
            mirror: "http://mirror:8080".into(),
            jdks: vec!["/opt/jdk/17".into(), "/opt/jdk/21".into()],
            init_scripts: "/etc/agent-run".into(),
            max_concurrent: 1,
            command: None,
        };
        let options = config.options().join(" ");
        assert!(options.contains("--no-problems-report"));
        assert!(options.contains("--init-script /etc/agent-run/redirect.init.gradle.kts --init-script /etc/agent-run/mirror.init.gradle.kts"));
        assert!(options.contains("--project-cache-dir /build-state/project-cache"));
        assert!(options.contains("-Pkotlin.project.persistent.dir=/build-state/kotlin"));
        assert!(options.contains("-Porg.gradle.java.installations.paths=/opt/jdk/17,/opt/jdk/21"));
        let env = config.environment();
        assert!(env.contains(&(
            "SDK_TEST_BASE_URL".into(),
            "http://mirror:8080/android-sdk/".into()
        )));
        assert!(env.contains(&(
            "ANDROID_USER_HOME".into(),
            "/build-state/android-user".into()
        )));
    }
}
