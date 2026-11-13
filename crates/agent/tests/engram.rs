//! A project's engram service in a real session: the checkout's
//! links resolve read-only, and the session commits a requested save on the host while it runs.
//! The service and its tools on their own: `crates/engram/tests/podman.rs`. On a scratch engram repository with a copy of engram-core and its built
//! server (`AGENT_RUN_TEST_ENGRAM_CORE`, default `~/Projects/MZLA/engram-work/engram-core`).
//! Needs the Podman machine. Run with `cargo test -p agent --test engram -- --ignored`.

mod common;

use agent::session;
use common::write;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Command, Output};

fn run(program: &str, args: &[&str], cwd: &Path) -> Output {
    // Fixture commands get none of the developer's Git configuration, templates or hooks.
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn exec(container: &str, cwd: &Path, script: &str) -> Output {
    Command::new("podman")
        .args(["exec", "-w"])
        .arg(cwd)
        .args([container, "sh", "-c", script])
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// MCP over the bridge, as the agent's CLI talks to it: newline-delimited JSON-RPC on stdio.
fn mcp(container: &str, calls: &[(&str, &str)]) -> String {
    let mut lines = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_owned(),
    ];
    for (i, (tool, arguments)) in calls.iter().enumerate() {
        lines.push(format!(r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments}}}}}"#, i + 2));
    }
    // The requests on stdin, not in the shell command: no quoting to get wrong.
    let mut child = Command::new("podman")
        .args([
            "exec",
            "-i",
            container,
            "sh",
            "-c",
            "(cat; sleep 10) | socat - UNIX-CONNECT:/run/engram/mcp.sock",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(
        &mut child.stdin.take().unwrap(),
        (lines.join("\n") + "\n").as_bytes(),
    )
    .unwrap();
    text(&child.wait_with_output().unwrap())
}

/// A scratch engram repository with project `app` (a copy of engram-core and its built server)
/// and a checkout linked to it the way engram links one, locked. Returns base, repository,
/// checkout and package.
fn fixture(
    name: &str,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let home = std::env::var("HOME").unwrap();
    let core = std::env::var("AGENT_RUN_TEST_ENGRAM_CORE")
        .unwrap_or(format!("{home}/Projects/MZLA/engram-work/engram-core"));
    let base = common::temp(name);
    // A scratch engram repository with one project.
    let repo = base.join("engram-work");
    fs::create_dir_all(&repo).unwrap();
    run(
        "cp",
        &["-Rpc", &core, repo.join("engram-core").to_str().unwrap()],
        &base,
    );
    let _ = fs::remove_dir_all(repo.join("engram-core/.git"));
    let _ = fs::remove_file(repo.join("engram-core/.git"));
    write(&repo.join("AGENTS.md"), "# engram\n");
    let package = repo.join("projects/app");
    write(&package.join("AGENTS.md"), "# app's engram context\n");
    write(&package.join(".memory/active-context.md"), "first state\n");
    write(
        &package.join(".claude/skills/demo/SKILL.md"),
        "---\nname: demo\n---\nA demo skill.\n",
    );
    run("git", &["init", "-q"], &repo);
    for (key, value) in [
        ("user.name", "test"),
        ("user.email", "test@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        run("git", &["config", key, value], &repo);
    }
    run("git", &["add", "projects"], &repo);
    run("git", &["commit", "-qm", "initial"], &repo);
    // A checkout linked to it the way engram links one, locked.
    let checkout = base.join("app");
    fs::create_dir_all(&checkout).unwrap();
    run("git", &["init", "-q"], &checkout);
    for (name, target) in [
        ("AGENTS.local.md", "../engram-work/projects/app/AGENTS.md"),
        (
            "CLAUDE.local.md",
            "../engram-work/engram-core/templates/CLAUDE.local.md",
        ),
        (".claude", "../engram-work/projects/app/.claude"),
        (".memory", "../engram-work/projects/app/.memory"),
    ] {
        symlink(target, checkout.join(name)).unwrap();
        run("chflags", &["-h", "uchg", name], &checkout);
    }

    (base, repo, checkout, package)
}

#[test]
#[ignore = "needs the Podman machine and engram-core's built server"]
fn engram_reads_and_writes_through_its_service_and_saves_on_the_host() {
    let (base, repo, checkout, package) = fixture("engram");
    let scratch = common::Scratch::new("engram");
    scratch.policy(&format!(
        "version = 1\n[projects.\"{}\"]\nengram = \"{}\"\n",
        checkout.display(),
        package.display()
    ));
    let session = session::start(
        "claude",
        &checkout,
        Some(&["sleep".into(), "600".into()]),
        scratch.paths(),
    )
    .expect("session starts");
    let agent = session.name.clone();
    let service = session.project.engram_container();
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        if !ok {
            failures.push(format!("{name}: {detail}"));
        }
    };

    let read = exec(
        &agent,
        &checkout,
        "cat AGENTS.local.md CLAUDE.local.md .claude/skills/demo/SKILL.md .memory/active-context.md",
    );
    let shown = text(&read);
    check(
        "the engram links resolve in the container",
        read.status.success()
            && shown.contains("app's engram context")
            && shown.contains("A demo skill"),
        shown,
    );
    for script in [
        "echo x >> .memory/active-context.md",
        "echo x >> AGENTS.local.md",
        "touch .claude/skills/planted",
    ] {
        check(
            "engram's mounts are read-only",
            !exec(&agent, &checkout, script).status.success(),
            script.to_owned(),
        );
    }
    // engram's own test covers the tools; here a note and its save go through a session.
    mcp(
        &agent,
        &[
            (
                "write_memory",
                r#"{"path":".memory/active-context.md","content":"written by the agent\n"}"#,
            ),
            (
                "save_memory",
                r#"{"message":"record the agent's state\n\nBecause the test says so."}"#,
            ),
        ],
    );

    // Committed during the session, within seconds, with the agent's body.
    let mut log = String::new();
    for _ in 0..30 {
        log = text(&run("git", &["log", "--format=%B", "-1"], &repo));
        if log.contains("record the agent's state") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    check(
        "the save is committed during the session",
        log.trim() == "context(app): record the agent's state\n\nBecause the test says so.",
        log,
    );
    session.end();
    let count = text(&run("git", &["rev-list", "--count", "HEAD"], &repo));
    check("nothing is committed twice", count.trim() == "2", count);
    let gone = Command::new("podman")
        .args(["container", "exists", &service])
        .status()
        .unwrap();
    check(
        "the service goes with the pod",
        !gone.success(),
        String::new(),
    );

    for name in ["AGENTS.local.md", "CLAUDE.local.md", ".claude", ".memory"] {
        let _ = Command::new("chflags")
            .args(["-h", "nouchg", name])
            .current_dir(&checkout)
            .status();
    }
    let _ = fs::remove_dir_all(&base);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "needs the Podman machine and engram-core's built server"]
fn a_slow_save_at_the_end_doesnt_block_session_starts() {
    let (base, repo, checkout, package) = fixture("engram-slow");
    // A save that hangs, as one waiting for a passphrase would.
    let save = repo.join("engram-core/scripts/engram-save.sh");
    let _ = fs::remove_file(&save);
    fs::write(&save, "#!/bin/sh\nsleep 20\n").unwrap();
    std::fs::set_permissions(&save, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let scratch = common::Scratch::new("engram-slow");
    scratch.policy(&format!(
        "version = 1\n[projects.\"{}\"]\nengram = \"{}\"\n",
        checkout.display(),
        package.display()
    ));
    let session = session::start(
        "claude",
        &checkout,
        Some(&["sleep".into(), "600".into()]),
        scratch.paths(),
    )
    .expect("session starts");
    // A recorded request, so the end saves.
    let requests = scratch.base.join("state/engram").join(&session.project.id);
    fs::create_dir_all(&requests).unwrap();
    fs::write(requests.join("requests"), "record\n").unwrap();
    let ending = std::thread::spawn(move || session.end());
    // Past the container's removal (up to 5 s), into the save.
    std::thread::sleep(std::time::Duration::from_secs(8));
    let lock = fs::File::create(scratch.base.join("state/start.lock")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut locked = false;
    while std::time::Instant::now() < deadline {
        if lock.try_lock().is_ok() {
            locked = true;
            let _ = lock.unlock();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    ending.join().unwrap();
    for name in ["AGENTS.local.md", "CLAUDE.local.md", ".claude", ".memory"] {
        let _ = Command::new("chflags")
            .args(["-h", "nouchg", name])
            .current_dir(&checkout)
            .status();
    }
    let _ = fs::remove_dir_all(&base);
    assert!(
        locked,
        "a session start would wait for the save at another session's end"
    );
}
