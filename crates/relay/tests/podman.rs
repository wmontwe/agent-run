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

/// An internal pod network and a client container on it with curl; returns the network's
/// subnets as `podman network inspect` prints them.
fn pod_with_client(podman: &Podman, ns: &podman::Namespace, pod: &str, client: &str) -> String {
    podman
        .run(&[
            "network",
            "create",
            "--internal",
            "--label",
            &ns.label(),
            pod,
        ])
        .unwrap();
    let image = image();
    podman
        .run(&[
            "run",
            "-d",
            "--name",
            client,
            "--label",
            &ns.label(),
            "--network",
            pod,
            "--read-only",
            "--cap-drop",
            "ALL",
            "--entrypoint",
            "sleep",
            &image,
            "300",
        ])
        .unwrap();
    podman
        .run(&[
            "network",
            "inspect",
            "--format",
            "{{range .Subnets}}{{.Subnet}} {{end}}",
            pod,
        ])
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
#[ignore = "needs the Podman machine and the internet"]
fn the_relay_reads_listed_repositories_only() {
    let podman = Podman::default();
    let id = std::process::id();
    let ns = podman::Namespace::new(&format!("relay-test-{id}"));
    let (pod, other_pod) = (ns.name("pod"), ns.name("other-pod"));
    let client = format!("relay-test-client-{id}");
    let other_client = format!("relay-test-other-client-{id}");
    let state_dir = std::env::temp_dir().join(format!("relay-test-{id}"));
    fs::create_dir_all(&state_dir).unwrap();
    let relay = Relay {
        service: relay::service(&podman, &ns, &state_dir),
        image: "localhost/agent-run-github-relay",
    };
    let container = relay.service.container.clone();
    let subnet = pod_with_client(&podman, &ns, &pod, &client);
    let other_subnet = pod_with_client(&podman, &ns, &other_pod, &other_client);
    let mut repos = vec!["octocat/Hello-World".to_owned()];
    // The first pod's project lists one more repository, for itself only.
    let mut pods = vec![
        relay::PodRepos {
            subnet,
            repos: vec!["octocat/git-consortium".to_owned()],
        },
        relay::PodRepos {
            subnet: other_subnet,
            repos: vec![],
        },
    ];
    relay.ensure(&repos, &pods).unwrap();
    relay.service.connect(&pod).unwrap();
    relay.service.connect(&other_pod).unwrap();
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
        (
            "the project's own repository, from its pod",
            status(&client, "/repos/octocat/git-consortium", &[]),
            "200",
        ),
        (
            "another project's repository, from another pod",
            status(&other_client, "/repos/octocat/git-consortium", &[]),
            "403",
        ),
        (
            "a global repository, from another pod",
            status(&other_client, repo, &[]),
            "200",
        ),
    ];

    // Changes to the lists apply live: a global one, and a project's own.
    repos.push("octocat/Spoon-Knife".to_owned());
    pods[1].repos.push("octocat/linguist".to_owned());
    relay.ensure(&repos, &pods).unwrap();
    sleep(Duration::from_secs(1));
    let after = status(&client, "/repos/octocat/Spoon-Knife", &[]);
    let after_own = status(&other_client, "/repos/octocat/linguist", &[]);
    let after_not_own = status(&client, "/repos/octocat/linguist", &[]);
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
    relay.service.disconnect(&other_pod).unwrap();
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
    assert_eq!(
        (after_own.as_str(), after_not_own.as_str()),
        ("200", "403"),
        "a project's added repository, from its pod and another, after a live change"
    );
}
