//! The public `agent` command: `agent <agent> <command> [args...]`
//! runs the command instead of the agent's CLI, arguments passed unchanged, its output on the
//! terminal and its exit status as `agent`'s. Needs the Podman machine.
//! Run with `cargo test -p agent --test cli -- --ignored`.

mod common;

use std::process::{Command, Stdio};

#[test]
#[ignore = "needs the Podman machine"]
fn a_command_runs_instead_of_the_agent_with_its_arguments_and_exit_status() {
    let root = common::checkout("cli");
    let scratch = common::Scratch::new("cli");
    scratch.policy("version = 1\n");
    let output = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args([
            "claude",
            "sh",
            "-c",
            "printf '%s|' \"$@\"; exit 7",
            "sh",
            "a b",
            "c",
        ])
        .current_dir(&root)
        .env("AGENT_RUN_HOME", &scratch.base)
        .env("AGENT_RUN_NAMESPACE", scratch.namespace.prefix())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("a b|c|"),
        "arguments: {stdout:?}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status.code(),
        Some(7),
        "exit status; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The namespace's project containers and pods still there; the shared services (proxy, relay,
/// mirror) stay by design.
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

#[test]
#[ignore = "needs the Podman machine"]
fn a_terminated_launcher_ends_its_session() {
    let root = common::checkout("cli-term");
    let scratch = common::Scratch::new("cli-term");
    scratch.policy("version = 1\n");
    let ns = scratch.namespace.prefix().to_owned();
    let mut launcher = Command::new(env!("CARGO_BIN_EXE_agent"))
        .args(["claude", "sleep", "600"])
        .current_dir(&root)
        .env("AGENT_RUN_HOME", &scratch.base)
        .env("AGENT_RUN_NAMESPACE", &ns)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    // Until the session's command runs.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !leftovers(&ns).contains("-claude-") && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    // Only the launcher, not its process group.
    Command::new("kill")
        .args(["-TERM", &launcher.id().to_string()])
        .status()
        .unwrap();
    let ended = std::time::Instant::now();
    let status = launcher.wait().unwrap();
    assert!(
        ended.elapsed() < std::time::Duration::from_secs(60),
        "the launcher took {:?}",
        ended.elapsed()
    );
    assert_ne!(status.code(), Some(0));
    assert_eq!(
        leftovers(&ns),
        "",
        "left behind after the launcher was terminated"
    );
    let users = std::fs::read_dir(scratch.base.join("state/proxy-users"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(users, 0, "the session's proxy credentials stayed");
    let _ = std::fs::remove_dir_all(&root);
}
