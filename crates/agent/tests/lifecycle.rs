//! A session's lifecycle around the launcher:
//! termination while it waits to start, a retained container's slot, a terminal that attaches
//! late or not at all. Needs the Podman machine.
//! Run with `cargo test -p agent --test lifecycle -- --ignored`.

mod common;

use agent::session;
use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// The namespace's project containers and pods; the shared services stay by design.
fn leftovers(namespace: &str) -> String {
    let filter = format!("label=agent-run.namespace={namespace}");
    let containers = Command::new("podman")
        .args(["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"])
        .output()
        .unwrap();
    let pods = Command::new("podman")
        .args(["pod", "ls", "--filter", &filter, "--format", "{{.Name}}"])
        .output()
        .unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&containers.stdout),
        String::from_utf8_lossy(&pods.stdout)
    );
    let shared = ["-egress", "-github-relay", "-mirror"];
    all.lines()
        .filter(|l| !shared.iter().any(|s| l.ends_with(s)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The child's status if it exits before the deadline; killed otherwise.
fn wait_for(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// A `podman` first on PATH that runs `attach` as `instead` says, everything else unchanged.
fn podman_wrapper(dir: &Path, instead: &str) -> String {
    let real = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v podman"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    fs::create_dir_all(dir).unwrap();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = attach ]; then {instead}; fi\nexec {} \"$@\"\n",
        real.trim()
    );
    fs::write(dir.join("podman"), script).unwrap();
    Command::new("chmod")
        .arg("+x")
        .arg(dir.join("podman"))
        .status()
        .unwrap();
    format!("{}:{}", dir.display(), std::env::var("PATH").unwrap())
}

fn launcher(
    root: &Path,
    scratch: &common::Scratch,
    path: &str,
    command: &[&str],
    stderr: &Path,
) -> Child {
    Command::new(env!("CARGO_BIN_EXE_agent"))
        .arg("claude")
        .args(command)
        .current_dir(root)
        .env("AGENT_RUN_HOME", &scratch.base)
        .env("AGENT_RUN_NAMESPACE", scratch.namespace.prefix())
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(fs::File::create(stderr).unwrap())
        .spawn()
        .unwrap()
}

#[test]
#[ignore = "needs the Podman machine"]
fn a_launcher_terminated_while_waiting_to_start_acquires_nothing() {
    let root = common::checkout("life-lock");
    let scratch = common::Scratch::new("life-lock");
    scratch.policy("version = 1\n");
    let log = scratch.base.join("stderr");
    // Another start holds the lock.
    let lock = pod::lock(&scratch.base.join("state")).unwrap();
    let mut child = launcher(
        &root,
        &scratch,
        &std::env::var("PATH").unwrap(),
        &["sleep", "600"],
        &log,
    );
    let deadline = Instant::now() + Duration::from_secs(300);
    while !fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("agent-run: images")
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_secs(1));
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    let status = wait_for(&mut child, Duration::from_secs(15));
    drop(lock);
    std::thread::sleep(Duration::from_secs(2));
    let stderr = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        status.is_some_and(|s| !s.success()),
        "the launcher didn't stop while waiting for the lock: {status:?}\n{stderr}"
    );
    assert_eq!(
        leftovers(scratch.namespace.prefix()),
        "",
        "acquired after it was told to stop\n{stderr}"
    );
    let users = fs::read_dir(scratch.base.join("state/proxy-users"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(users, 0, "proxy credentials after it was told to stop");
    let _ = fs::remove_dir_all(&root);
}

#[test]
#[ignore = "needs the Podman machine"]
fn a_terminal_attaching_late_misses_nothing() {
    let root = common::checkout("life-late");
    let scratch = common::Scratch::new("life-late");
    scratch.policy("version = 1\n");
    let path = podman_wrapper(&scratch.base.join("bin"), "sleep 2");
    let log = scratch.base.join("stderr");
    let mut child = launcher(
        &root,
        &scratch,
        &path,
        &["sh", "-c", "echo first line; exit 3"],
        &log,
    );
    let status = wait_for(&mut child, Duration::from_secs(300));
    let mut stdout = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
    let stderr = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        stdout.contains("first line"),
        "output before the terminal attached: {stdout:?}\n{stderr}"
    );
    assert!(
        !stdout.contains('\u{1b}'),
        "the gate's announcement reached the terminal: {stdout:?}"
    );
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(3),
        "exit status\n{stderr}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
#[ignore = "needs the Podman machine"]
fn a_terminal_that_fails_to_attach_ends_the_session() {
    let root = common::checkout("life-gone");
    let scratch = common::Scratch::new("life-gone");
    scratch.policy("version = 1\n");
    let path = podman_wrapper(
        &scratch.base.join("bin"),
        "echo 'attach refused' >&2; exit 125",
    );
    let log = scratch.base.join("stderr");
    let mut child = launcher(&root, &scratch, &path, &["sleep", "600"], &log);
    let status = wait_for(&mut child, Duration::from_secs(300));
    let stderr = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        status.is_some_and(|s| !s.success()),
        "the launcher waited for the session: {status:?}\n{stderr}"
    );
    assert!(
        stderr.contains("podman attach ended"),
        "no diagnostic: {stderr}"
    );
    assert_eq!(leftovers(scratch.namespace.prefix()), "", "left behind");
    let _ = fs::remove_dir_all(&root);
}

#[test]
#[ignore = "needs the Podman machine"]
fn an_exited_session_keeps_its_display_until_it_ends() {
    let root = common::checkout("life-slot");
    let scratch = common::Scratch::new("life-slot");
    scratch.policy("version = 1\n");
    let done = session::start("claude", &root, Some(&["true".to_owned()]), scratch.paths())
        .expect("a session");
    let podman = podman::Podman::default();
    let deadline = Instant::now() + Duration::from_secs(30);
    while podman
        .run(&["inspect", "--format", "{{.State.Status}}", &done.name])
        .unwrap()
        .trim()
        != "exited"
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(200));
    }
    let next = vision::free_display(&podman, &done.project).unwrap();
    done.end();
    let _ = fs::remove_dir_all(&root);
    assert_ne!(
        next, 1,
        "an exited session's display (and name) was free before it ended"
    );
}

/// The namespace's containers, pods and networks, shared services included.
fn running(namespace: &podman::Namespace) -> String {
    let filter = format!("label={}", namespace.label());
    let list = |args: &[&str]| {
        String::from_utf8(Command::new("podman").args(args).output().unwrap().stdout).unwrap()
    };
    [
        list(&["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"]),
        list(&["pod", "ls", "--filter", &filter, "--format", "{{.Name}}"]),
        list(&[
            "network",
            "ls",
            "--filter",
            &filter,
            "--format",
            "{{.Name}}",
        ]),
    ]
    .concat()
    .trim()
    .to_owned()
}

fn exists(kind: &str, name: &str) -> bool {
    Command::new("podman")
        .args([kind, "exists", name])
        .status()
        .unwrap()
        .success()
}

#[test]
#[ignore = "needs the Podman machine"]
fn shutdown_stops_the_namespace_keeps_its_data_and_spares_running_sessions_unless_forced() {
    let root = common::checkout("life-down");
    let scratch = common::Scratch::new("life-down");
    scratch.policy("version = 1\n");
    let sleep = ["sleep".to_owned(), "600".to_owned()];
    // Not agent-run's: shutdown leaves it alone.
    let foreign = format!(
        "{}-foreign",
        scratch.namespace.prefix().replace("agent-test", "other")
    );
    Command::new("podman")
        .args([
            "run",
            "-d",
            "--name",
            &foreign,
            "--entrypoint",
            "sleep",
            "localhost/agent-run-build",
            "300",
        ])
        .output()
        .unwrap();
    let mut failures = Vec::new();

    let active = session::start("claude", &root, Some(&sleep), scratch.paths()).expect("a session");
    let volume = active.project.state_volume();
    match session::shutdown(&scratch.paths(), false) {
        Ok(()) => failures.push("shutdown ended a running session without --force".to_owned()),
        Err(e) if !e.contains("--force") => {
            failures.push(format!("the refusal doesn't name --force: {e}"))
        }
        Err(_) => {}
    }
    if !exists("container", &active.name) {
        failures.push("a refused shutdown stopped the session".to_owned());
    }
    session::shutdown(&scratch.paths(), true).expect("forced shutdown");
    if !running(&scratch.namespace).is_empty() {
        failures.push(format!(
            "after a forced shutdown: {}",
            running(&scratch.namespace)
        ));
    }
    if !exists("volume", &volume) {
        failures.push(format!("the project's volume {volume} is gone"));
    }
    active.end();

    // A later start works, and an idle namespace shuts down without --force.
    let again = session::start("claude", &root, Some(&sleep), scratch.paths())
        .expect("a start after shutdown");
    again.end();
    session::shutdown(&scratch.paths(), false).expect("an idle shutdown");
    if !running(&scratch.namespace).is_empty() {
        failures.push(format!(
            "after an idle shutdown: {}",
            running(&scratch.namespace)
        ));
    }
    if !exists("container", &foreign) {
        failures.push("a container outside the namespace was removed".to_owned());
    }
    let _ = Command::new("podman")
        .args(["rm", "--force", "--time", "0", &foreign])
        .output();
    let _ = fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
