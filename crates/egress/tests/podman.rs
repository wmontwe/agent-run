//! The egress proxy against real Podman and the internet.
//! Needs the Podman machine, `localhost/agent-run-proxy`
//! and an image with bash, curl and node (`AGENT_RUN_TEST_IMAGE`, default
//! `localhost/agent-run-claude`). Run with `cargo test -p egress -- --ignored`.

use egress::{Egress, PodEgress, SessionEgress, denials, htpasswd_line, subnet};
use podman::Podman;
use policy::Destination;
use std::fs;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

fn image() -> String {
    std::env::var("AGENT_RUN_TEST_IMAGE")
        .unwrap_or_else(|_| "localhost/agent-run-claude".to_owned())
}

fn destination(host: &str) -> Destination {
    Destination {
        host: host.to_owned(),
        port: 443,
    }
}

/// Whether `script` succeeds in `container` within 8 seconds.
fn succeeds(container: &str, script: &str) -> bool {
    Command::new("podman")
        .args(["exec", container, "timeout", "8", "bash", "-c", script])
        .output()
        .unwrap()
        .status
        .success()
}

/// `"<CONNECT status> <HTTP status>"` of `curl` in `container`, through the proxy unless
/// `direct`. curl exits non-zero on refusals, so its output is read whatever the status.
fn fetch(container: &str, url: &str, direct: bool) -> String {
    let mut args = vec![
        "exec",
        container,
        "curl",
        "-s",
        "-o",
        "/dev/null",
        "--max-time",
        "8",
        "-w",
        "%{http_connect} %{http_code}",
    ];
    args.extend(if direct {
        ["--noproxy", "*"]
    } else {
        ["-x", "http://proxy:3128"]
    });
    args.push(url);
    let output = Command::new("podman").args(&args).output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A container hardened like an agent's, on `network`, with the proxy variables set.
fn run(podman: &Podman, label: &str, name: &str, network: &str, extra: &[&str], command: &[&str]) {
    let image = image();
    let mut args = vec![
        "run",
        "-d",
        "--name",
        name,
        "--label",
        label,
        "--network",
        network,
        "--read-only",
        "--cap-drop",
        "ALL",
    ];
    args.extend([
        "--security-opt",
        "no-new-privileges",
        "-e",
        "HTTPS_PROXY=http://proxy:3128",
    ]);
    args.extend(extra);
    args.push(&image);
    args.extend(command);
    podman.run(&args).unwrap();
}

#[test]
#[ignore = "needs the Podman machine and the internet"]
fn one_proxy_enforces_each_pods_allowlist() {
    let podman = Podman::default();
    let id = std::process::id();
    let ns = podman::Namespace::new(&format!("egress-test-{id}"));
    let (pod_a, pod_b) = (ns.name("a"), ns.name("b"));
    let (client, server, internal) = (ns.name("client"), ns.name("server"), ns.name("internal"));
    for net in [&pod_a, &pod_b] {
        podman
            .run(&[
                "network",
                "create",
                "--internal",
                "--label",
                &ns.label(),
                net,
            ])
            .unwrap();
    }
    let state_dir = std::env::temp_dir().join(format!("egress-test-{id}"));
    fs::create_dir_all(&state_dir).unwrap();
    let proxy = Egress {
        service: egress::service(&podman, &ns, &state_dir),
        image: "localhost/agent-run-proxy",
        mirror: None,
    };
    let (container, network) = (
        proxy.service.container.clone(),
        proxy.service.network.clone(),
    );

    let mut pods = vec![
        PodEgress {
            subnet: subnet(&podman, &pod_a).unwrap(),
            allow: vec![
                destination("example.com"),
                destination("internal.example.com"),
            ],
            sessions: vec![],
        },
        PodEgress {
            subnet: subnet(&podman, &pod_b).unwrap(),
            allow: vec![destination("example.org")],
            sessions: vec![],
        },
    ];
    proxy.ensure(&pods, &[]).unwrap();
    proxy.service.connect(&pod_a).unwrap();
    proxy.service.connect(&pod_b).unwrap();

    run(
        &podman,
        &ns.label(),
        &client,
        &pod_a,
        &["--entrypoint", "sleep"],
        &["300"],
    );
    // A server in the other pod, to show pods can't reach each other.
    let listen = r#"require("http").createServer((q, r) => r.end("b")).listen(8080, "0.0.0.0")"#;
    run(
        &podman,
        &ns.label(),
        &server,
        &pod_b,
        &["--entrypoint", "node"],
        &["-e", listen],
    );
    // An allowed name that resolves to a private address: another container on pod A's
    // network, found through Podman's DNS. Public names for 127.0.0.1 aren't reliable:
    // resolvers with rebinding protection drop such answers.
    run(
        &podman,
        &ns.label(),
        &internal,
        &pod_a,
        &[
            "--network-alias",
            "internal.example.com",
            "--entrypoint",
            "sleep",
        ],
        &["300"],
    );
    sleep(Duration::from_secs(2));
    let address = |object: &str, format: &str| {
        podman
            .run(&["inspect", "--format", format, object])
            .unwrap()
            .trim()
            .to_owned()
    };
    let server_ip = address(
        &server,
        "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
    );
    let gateway = podman
        .run(&[
            "network",
            "inspect",
            "--format",
            "{{range .Subnets}}{{.Gateway}}{{end}}",
            &pod_a,
        ])
        .unwrap()
        .trim()
        .to_owned();

    // Controls: what the checks below refuse is reachable without the isolation.
    let control_server = podman
        .run(&[
            "run",
            "--rm",
            "--network",
            &pod_b,
            "--entrypoint",
            "curl",
            &image(),
            "-sf",
            "--max-time",
            "8",
            &format!("http://{server_ip}:8080"),
        ])
        .is_ok();
    let control_outside = podman
        .run(&[
            "run",
            "--rm",
            "--network",
            &network,
            "--entrypoint",
            "curl",
            &image(),
            "-sf",
            "-o",
            "/dev/null",
            "--max-time",
            "8",
            "https://example.com",
        ])
        .is_ok();

    let statuses = [
        (
            "allowed host",
            fetch(&client, "https://example.com", false),
            "200 200",
        ),
        (
            "other host",
            fetch(&client, "https://example.org", false),
            "403 000",
        ),
        (
            "allowed host without the proxy",
            fetch(&client, "https://example.com", true),
            "000 000",
        ),
        (
            "literal IPv4",
            fetch(&client, "https://1.1.1.1", false),
            "403 000",
        ),
        (
            "literal IPv6",
            fetch(&client, "https://[2606:4700:4700::1111]", false),
            "403 000",
        ),
        (
            "allowed name resolving to a private address",
            fetch(&client, "https://internal.example.com", false),
            "403 000",
        ),
        (
            "plain HTTP",
            fetch(&client, "http://example.com", false),
            "000 403",
        ),
        (
            "the other pod's host",
            fetch(&server, "https://example.com", false),
            "403 000",
        ),
        (
            "the other pod's own host",
            fetch(&server, "https://example.org", false),
            "200 200",
        ),
    ];
    let refused = [
        (
            "literal IPv4 without the proxy",
            "curl -sf -o /dev/null --noproxy '*' https://1.1.1.1".to_owned(),
        ),
        (
            "literal IPv6 without the proxy",
            "curl -sf -o /dev/null --noproxy '*' 'https://[2606:4700:4700::1111]'".to_owned(),
        ),
        ("outside DNS lookup", "getent hosts example.com".to_owned()),
        (
            "UDP to 8.8.8.8:53",
            "echo x > /dev/udp/8.8.8.8/53".to_owned(),
        ),
        (
            "the other pod directly",
            format!("curl -sf --noproxy '*' http://{server_ip}:8080"),
        ),
        (
            "the other pod through the proxy",
            format!("curl -sf --proxytunnel -x http://proxy:3128 http://{server_ip}:8080"),
        ),
        (
            "the network's gateway",
            format!("echo > /dev/tcp/{gateway}/22"),
        ),
        (
            "host.containers.internal",
            "echo > /dev/tcp/host.containers.internal/22".to_owned(),
        ),
        (
            "gvproxy's gateway",
            "echo > /dev/tcp/192.168.127.1/22".to_owned(),
        ),
        (
            "gvproxy's host",
            "echo > /dev/tcp/192.168.127.254/80".to_owned(),
        ),
        (
            "Squid's cache manager",
            "curl -sf --noproxy '*' http://proxy:3128/squid-internal-mgr/info".to_owned(),
        ),
    ];
    let mut failures = Vec::new();
    for (name, ok) in [
        (
            "control: the other pod's server from its own network",
            control_server,
        ),
        (
            "control: the internet from the outbound network",
            control_outside,
        ),
    ] {
        if !ok {
            failures.push(name.to_owned());
        }
    }
    for (name, actual, expected) in statuses {
        if actual != expected {
            failures.push(format!("{name}: {actual}, expected {expected}"));
        }
    }
    for (name, script) in &refused {
        if succeeds(&client, script) {
            failures.push(format!("not refused: {name}"));
        }
    }
    let header = "X-Agent-Run-Denied: example.org:443 is not allowed by agent-run's policy; to approve, run on the host: agent allow example.org:443";
    if !succeeds(
        &client,
        &format!("curl -sv --max-time 8 https://example.org 2>&1 | grep -qF \"{header}\""),
    ) {
        failures.push(
            "a refusal doesn't name the destination and the approve command (X-Agent-Run-Denied)"
                .into(),
        );
    }
    let denied = denials(&proxy.service.log().unwrap(), &pods[0].subnet, 20);
    if !denied.iter().any(|d| d.destination == "example.org:443") {
        let log = proxy.service.log().unwrap();
        let tail: Vec<&str> = log.lines().rev().take(8).collect();
        failures.push(format!(
            "example.org:443 missing from pod A's denials: {denied:?}; log ends:\n{}",
            tail.join("\n")
        ));
    }
    if denials(&proxy.service.log().unwrap(), &pods[1].subnet, 20)
        .iter()
        .any(|d| d.destination == "1.1.1.1:443")
    {
        failures.push("pod A's refusal listed for pod B".into());
    }

    // Sessions: each identity reaches its own destinations and local services; a request
    // without credentials (a build's) or with another's gets the shared list only.
    let local = Destination {
        host: server_ip.clone(),
        port: 8080,
    };
    pods[0].sessions = vec![
        SessionEgress {
            user: "pi-1".into(),
            allow: vec![destination("example.org")],
            local: vec![local],
        },
        SessionEgress {
            user: "claude-1".into(),
            allow: vec![],
            local: vec![],
        },
    ];
    proxy
        .ensure(
            &pods,
            &[
                htpasswd_line("pi-1", "secret-pi", "salt1"),
                htpasswd_line("claude-1", "secret-claude", "salt2"),
            ],
        )
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    let with = |user: &str, url: &str| {
        let script = format!(
            "curl -s -o /dev/null --max-time 8 -w '%{{http_connect}} %{{http_code}}' -x http://{user}@proxy:3128 {url}"
        );
        let output = Command::new("podman")
            .args(["exec", &client, "sh", "-c", &script])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    for (name, actual, expected) in [
        (
            "an identity's own host",
            with("pi-1:secret-pi", "https://example.org"),
            "200 200",
        ),
        (
            "another identity's host",
            with("claude-1:secret-claude", "https://example.org"),
            "403 000",
        ),
        (
            "a wrong password",
            with("pi-1:wrong", "https://example.org"),
            "407 000",
        ),
        (
            "no credentials",
            fetch(&client, "https://example.org", false),
            "403 000",
        ),
        (
            "the shared list with credentials",
            with("claude-1:secret-claude", "https://example.com"),
            "200 200",
        ),
        (
            "a local service, plain HTTP",
            with("pi-1:secret-pi", &format!("http://{server_ip}:8080/")),
            "000 200",
        ),
        (
            "the same host on another port",
            with("pi-1:secret-pi", &format!("http://{server_ip}:8081/")),
            "000 403",
        ),
        (
            "another identity's local service",
            with(
                "claude-1:secret-claude",
                &format!("http://{server_ip}:8080/"),
            ),
            "000 403",
        ),
    ] {
        if actual != expected {
            failures.push(format!("{name}: {actual}, expected {expected}"));
        }
    }
    let denied = denials(&proxy.service.log().unwrap(), &pods[0].subnet, 20);
    if !denied.iter().any(|d| d.agent() == Some("claude")) {
        failures.push(format!("denials name the agent: {denied:?}"));
    }
    pods[0].sessions.clear();

    // A change applies live, without restarting anything.
    pods[1].allow.push(destination("example.com"));
    proxy.ensure(&pods, &[]).unwrap();
    sleep(Duration::from_secs(1));
    if fetch(&server, "https://example.com", false) != "200 200" {
        failures.push("not allowed after a live change".into());
    }

    // Fail closed: without the proxy nothing gets out.
    podman.run(&["stop", "-t", "2", &container]).unwrap();
    for (name, direct) in [
        ("proxy stopped: allowed host", false),
        ("proxy stopped: allowed host without the proxy", true),
    ] {
        let status = fetch(&client, "https://example.com", direct);
        if status.ends_with("200") {
            failures.push(format!("{name}: {status}"));
        }
    }
    podman.run(&["start", &container]).unwrap();
    sleep(Duration::from_secs(3));
    if fetch(&client, "https://example.com", false) != "200 200" {
        failures.push("proxy restarted: allowed host not reachable".into());
    }

    ns.remove_all(&podman, &[]);
    let _ = fs::remove_dir_all(&state_dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
