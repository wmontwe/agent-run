//! Codex, Gemini and Pi in real sessions: each starts behind the gate, its CLI
//! sees the session's browser through its own configuration mechanism, and nothing of it lands in
//! the agent's home. Needs the Podman machine; builds the agents' images if needed.
//! Run with `cargo test -p agent --test agents -- --ignored`.

mod common;

use agent::session;
use agents::{Agent, Server};
use std::fs;
use std::process::Command;

fn listing(container: &str, checkout: &std::path::Path, command: &[String]) -> String {
    let output = Command::new("podman")
        .args(["exec", "-w"])
        .arg(checkout)
        .arg(container)
        .args(command)
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
#[ignore = "needs the Podman machine"]
fn each_agent_sees_its_sessions_browser_and_keeps_its_home() {
    let root = common::checkout("agents");
    let scratch = common::Scratch::new("agents");
    scratch.policy("version = 1\n");
    let mut failures = Vec::new();
    for agent in [Agent::Codex, Agent::Gemini, Agent::Pi] {
        let paths = scratch.paths_for(agent.name());
        let home = paths.agent_home.clone();
        let session = session::start(
            agent.name(),
            &root,
            Some(&["sleep".into(), "600".into()]),
            paths,
        )
        .expect("session starts");
        let url = vision::browser_url(session.display);
        // Each CLI's own listing of its MCP servers, as the session configured them.
        let command: Vec<String> = match agent {
            Agent::Codex => {
                let servers = [Server::Http {
                    name: "playwright".into(),
                    url: url.clone(),
                }];
                let mut command = agent.launch(&servers, &home).unwrap().command;
                command.extend(["mcp".into(), "list".into()]);
                command
            }
            other => vec![other.name().into(), "mcp".into(), "list".into()],
        };
        let listed = listing(&session.name, &root, &command);
        if !(listed.contains("playwright") && listed.contains(&url)) {
            failures.push(format!(
                "{}: the browser isn't configured:\n{}",
                agent.name(),
                listed.chars().take(600).collect::<String>()
            ));
        }
        session.end();
        // agent-run writes nothing about its servers into the agent's home.
        let written = fs::read_dir(&home).map(|d| d.count()).unwrap_or(0) > 0
            && Command::new("grep")
                .args(["-rlq", &url])
                .arg(&home)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        if written {
            failures.push(format!(
                "{}: the session's browser was written into its home",
                agent.name()
            ));
        }
    }
    let _ = fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
