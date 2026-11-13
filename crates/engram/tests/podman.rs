//! The engram service on its own: engram's server behind the
//! socket, its tools, no network, and the host's save with summary and body. On a scratch engram
//! repository with a copy of engram-core and its built server (`AGENT_RUN_TEST_ENGRAM_CORE`,
//! default `~/Projects/MZLA/engram-work/engram-core`). Needs the Podman machine and
//! `localhost/agent-run-build`. Run with `cargo test -p engram -- --ignored`.

use engram::Engram;
use pod::Project;
use podman::{Namespace, Podman};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const IMAGE: &str = "localhost/agent-run-build";

fn run(program: &str, args: &[&str], cwd: &Path) -> String {
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
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A scratch engram repository with the project `app`, committed.
fn repository(base: &Path) -> PathBuf {
    let home = std::env::var("HOME").unwrap();
    let core = std::env::var("AGENT_RUN_TEST_ENGRAM_CORE")
        .unwrap_or(format!("{home}/Projects/MZLA/engram-work/engram-core"));
    let repo = base.join("engram-work");
    fs::create_dir_all(repo.join("projects/app/.memory")).unwrap();
    run(
        "cp",
        &["-Rpc", &core, repo.join("engram-core").to_str().unwrap()],
        base,
    );
    let _ = fs::remove_dir_all(repo.join("engram-core/.git"));
    let _ = fs::remove_file(repo.join("engram-core/.git"));
    fs::write(repo.join("AGENTS.md"), "# engram\n").unwrap();
    fs::write(repo.join("projects/app/AGENTS.md"), "# app\n").unwrap();
    fs::write(
        repo.join("projects/app/.memory/active-context.md"),
        "first state\n",
    )
    .unwrap();
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
    repo
}

/// MCP over the socket, as an agent's bridge talks to it: newline-delimited JSON-RPC.
fn mcp(client: &str, calls: &[(&str, &str)]) -> String {
    let mut lines = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_owned(),
    ];
    for (i, (tool, arguments)) in calls.iter().enumerate() {
        lines.push(format!(r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments}}}}}"#, i + 2));
    }
    let mut child = Command::new("podman")
        .args([
            "exec",
            "-i",
            client,
            "sh",
            "-c",
            "(cat; sleep 8) | socat - UNIX-CONNECT:/run/engram/mcp.sock",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all((lines.join("\n") + "\n").as_bytes())
        .unwrap();
    text(&child.wait_with_output().unwrap())
}

/// Removes the test's namespace when the test ends, also when it fails.
struct Cleanup(Namespace);

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.remove_all(&Podman::default(), &[]);
    }
}

#[test]
#[ignore = "needs the Podman machine and engram-core's built server"]
fn the_service_serves_the_tools_and_the_host_commits_its_saves() {
    let podman = Podman::default();
    let base = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("engram-service-{}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    let repo = repository(&base);
    let ns = Namespace::new(&format!("engram-test-{}", std::process::id()));
    let _cleanup = Cleanup(ns.clone());
    let project = Project::new(&ns, &base.join("app"));
    let engram = Engram::resolve(&repo.join("projects/app")).unwrap();
    let requests = base.join("requests");
    engram.ensure(&podman, &project, IMAGE, &requests).unwrap();
    // A client on the socket volume, as an agent's container is.
    let client = ns.name("client");
    let socket = format!("{}:/run/engram:z", project.engram_volume());
    podman
        .run(&[
            "run",
            "-d",
            "--name",
            &client,
            "--label",
            &ns.label(),
            "--userns",
            "keep-id:uid=1000,gid=1000",
            "-v",
            &socket,
            "--entrypoint",
            "sleep",
            IMAGE,
            "300",
        ])
        .unwrap();
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        if !ok {
            failures.push(format!("{name}: {detail}"));
        }
    };

    let network = podman
        .run(&[
            "inspect",
            "--format",
            "{{.HostConfig.NetworkMode}}",
            &project.engram_container(),
        ])
        .unwrap();
    check(
        "the service has no network",
        network.trim() == "none",
        network,
    );
    let answers = mcp(
        &client,
        &[
            ("read_memory", r#"{"path":".memory/active-context.md"}"#),
            (
                "write_memory",
                r#"{"path":".memory/active-context.md","content":"written through the service\n"}"#,
            ),
            (
                "save_memory",
                r#"{"message":"record the state\n\nBecause the test says so."}"#,
            ),
        ],
    );
    check(
        "read_memory",
        answers.contains("first state"),
        answers.chars().take(600).collect(),
    );
    check(
        "save_memory records the request",
        answers.contains("committed on the host"),
        answers.chars().take(600).collect(),
    );
    let written =
        fs::read_to_string(repo.join("projects/app/.memory/active-context.md")).unwrap_or_default();
    check(
        "write_memory writes the repository",
        written == "written through the service\n",
        written,
    );

    let saved = engram.save(&requests);
    check(
        "the host commits the request",
        saved == Ok(Some("record the state".into())),
        format!("{saved:?}"),
    );
    let message = run("git", &["log", "--format=%B", "-1"], &repo);
    check(
        "with summary and body",
        message.trim() == "context(app): record the state\n\nBecause the test says so.",
        message,
    );
    check(
        "a request is committed once",
        engram.save(&requests) == Ok(None),
        String::new(),
    );

    Engram::remove(&podman, &project);
    let gone = podman
        .exists(podman::Object::Container, &project.engram_container())
        .unwrap();
    check("remove stops the service", !gone, String::new());
    let _ = fs::remove_dir_all(&base);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
