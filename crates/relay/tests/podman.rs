//! The GitHub relay against real Podman and api.github.com. Needs the Podman machine,
//! `localhost/agent-run-github-relay` (`podman build -t localhost/agent-run-github-relay
//! proxy/github-relay`) and an image with curl (`AGENT_RUN_TEST_IMAGE`, default
//! `localhost/agent-run-claude`). Run with `cargo test -p relay -- --ignored`.

use podman::Podman;
use relay::Relay;
use std::fs;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

fn image() -> String {
    std::env::var("AGENT_RUN_TEST_IMAGE")
        .unwrap_or_else(|_| "localhost/agent-run-claude".to_owned())
}

/// The HTTP status of a request to the relay from `client`; `extra` are curl's options.
fn status(client: &str, path: &str, extra: &[&str]) -> String {
    let url = format!("{}{path}", relay::URL);
    let output = Command::new("podman")
        .args([
            "exec",
            client,
            "curl",
            "-s",
            "-o",
            "/dev/null",
            "--max-time",
            "10",
            "--path-as-is",
            "-w",
            "%{http_code}",
        ])
        .args(extra)
        .arg(&url)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
#[ignore = "needs the Podman machine and the internet"]
fn the_relay_reads_listed_repositories_only() {
    let podman = Podman::default();
    let id = std::process::id();
    let ns = podman::Namespace::new(&format!("relay-test-{id}"));
    let pod = ns.name("pod");
    let client = format!("relay-test-client-{id}");
    podman
        .run(&[
            "network",
            "create",
            "--internal",
            "--label",
            &ns.label(),
            &pod,
        ])
        .unwrap();
    let state_dir = std::env::temp_dir().join(format!("relay-test-{id}"));
    fs::create_dir_all(&state_dir).unwrap();
    let relay = Relay {
        service: relay::service(&podman, &ns, &state_dir),
        image: "localhost/agent-run-github-relay",
    };
    let container = relay.service.container.clone();
    let mut repos = vec!["octocat/Hello-World".to_owned()];
    relay.ensure(&repos).unwrap();
    relay.service.connect(&pod).unwrap();
    let image = image();
    podman
        .run(&[
            "run",
            "-d",
            "--name",
            &client,
            "--label",
            &ns.label(),
            "--network",
            &pod,
            "--read-only",
            "--cap-drop",
            "ALL",
            "--entrypoint",
            "sleep",
            &image,
            "300",
        ])
        .unwrap();
    sleep(Duration::from_secs(2));

    let repo = "/repos/octocat/Hello-World";
    let checks = [
        ("listed repository", status(&client, repo, &[]), "200"),
        (
            "listed repository, HEAD",
            status(&client, repo, &["-I"]),
            "200",
        ),
        (
            "listed repository, a subpath and query",
            status(&client, &format!("{repo}/pulls?state=open&per_page=1"), &[]),
            "200",
        ),
        (
            "the agent's own token is dropped",
            status(&client, repo, &["-H", "Authorization: token not-a-token"]),
            "200",
        ),
        (
            "listed name with a different case",
            status(&client, "/repos/OCTOCAT/hello-world", &[]),
            "200",
        ),
        (
            "unlisted repository",
            status(&client, "/repos/octocat/Spoon-Knife", &[]),
            "403",
        ),
        (
            "a name that starts like a listed one",
            status(&client, &format!("{repo}-fork"), &[]),
            "403",
        ),
        ("other API paths", status(&client, "/user", &[]), "403"),
        (
            "POST",
            status(
                &client,
                &format!("{repo}/issues"),
                &["-X", "POST", "-d", "{}"],
            ),
            "403",
        ),
        (
            "leaving the repository with ..",
            status(&client, &format!("{repo}/../../../user"), &[]),
            "403",
        ),
        (
            "leaving the repository with %2e%2e",
            status(&client, &format!("{repo}/%2e%2e/%2e%2e/%2e%2e/user"), &[]),
            "403",
        ),
        (
            "leaving the repository with a backslash",
            status(&client, &format!("{repo}\\..\\..\\..\\user"), &[]),
            "403",
        ),
    ];

    // A change to the list applies live.
    repos.push("octocat/Spoon-Knife".to_owned());
    relay.ensure(&repos).unwrap();
    sleep(Duration::from_secs(1));
    let after = status(&client, "/repos/octocat/Spoon-Knife", &[]);
    let restarts = podman
        .run(&[
            "container",
            "inspect",
            "--format",
            "{{.RestartCount}} {{.State.StartedAt}}",
            &container,
        ])
        .unwrap();

    relay.service.disconnect(&pod).unwrap();
    ns.remove_all(&podman, &[]);
    let _ = fs::remove_dir_all(&state_dir);
    let failures: Vec<String> = checks
        .iter()
        .filter(|(_, actual, expected)| actual != expected)
        .map(|(name, actual, expected)| format!("{name}: {actual}, expected {expected}"))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(
        after, "200",
        "added repository, after a live change ({restarts})"
    );
}
