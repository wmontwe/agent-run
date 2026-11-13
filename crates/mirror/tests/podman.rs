//! The mirror against real Podman and the internet. Needs the Podman machine,
//! `localhost/agent-run-mirror` (`podman build -t localhost/agent-run-mirror proxy/mirror`),
//! `localhost/agent-run-proxy` and an image with curl (`AGENT_RUN_TEST_IMAGE`, default
//! `localhost/agent-run-claude`). Run with `cargo test -p mirror -- --ignored`.

use egress::{Egress, PodEgress, subnet};
use mirror::{Mirror, hosts};
use podman::Podman;
use policy_free::destinations;
use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

mod policy_free {
    /// Port-443 destinations for hosts.
    pub fn destinations(hosts: &[String]) -> Vec<policy::Destination> {
        hosts
            .iter()
            .map(|h| policy::Destination {
                host: h.clone(),
                port: 443,
            })
            .collect()
    }
}

fn image() -> String {
    std::env::var("AGENT_RUN_TEST_IMAGE")
        .unwrap_or_else(|_| "localhost/agent-run-claude".to_owned())
}

/// The HTTP status of `curl` in `container`; `extra` are curl's options.
fn status(container: &str, url: &str, extra: &[&str]) -> String {
    let output = Command::new("podman")
        .args([
            "exec",
            container,
            "curl",
            "-s",
            "-o",
            "/dev/null",
            "--max-time",
            "60",
            "-w",
            "%{http_connect} %{http_code}",
        ])
        .args(extra)
        .arg(url)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
#[ignore = "needs the Podman machine and the internet"]
fn builds_read_through_the_mirror_and_the_mirror_reaches_its_remotes_only() {
    let podman = Podman::default();
    let id = std::process::id();
    let ns = podman::Namespace::new(&format!("mirror-test-{id}"));
    let (pod, client, inside) = (ns.name("pod"), ns.name("client"), ns.name("inside"));
    let state_dir = std::env::temp_dir().join(ns.name("state"));
    fs::create_dir_all(&state_dir).unwrap();
    let remotes: BTreeMap<String, String> = [(
        "central".to_owned(),
        "https://repo.maven.apache.org/maven2".to_owned(),
    )]
    .into();

    let mirror = Mirror {
        service: mirror::service(&podman, &ns, &state_dir),
        volume: ns.name("mirror-data"),
        image: "localhost/agent-run-mirror",
    };
    mirror.service.network().unwrap();
    let (container, network) = (
        mirror.service.container.clone(),
        mirror.service.network.clone(),
    );
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
    let proxy = Egress {
        service: egress::service(&podman, &ns, &state_dir),
        image: "localhost/agent-run-proxy",
        mirror: Some(("mirror", 8080)),
    };
    let pods = [
        PodEgress {
            subnet: subnet(&podman, &network).unwrap(),
            allow: destinations(&hosts(&remotes)),
            sessions: vec![],
        },
        PodEgress {
            subnet: subnet(&podman, &pod).unwrap(),
            allow: vec![],
            sessions: vec![],
        },
    ];
    proxy.ensure(&pods, &[]).unwrap();
    proxy.service.connect(&network).unwrap();
    proxy.service.connect(&pod).unwrap();
    mirror
        .ensure(
            &remotes,
            &format!("{}:3128", proxy.service.address_on(&network).unwrap()),
        )
        .unwrap();
    mirror.service.connect(&pod).unwrap();
    let image = image();
    for (name, net) in [(&client, &pod), (&inside, &network)] {
        podman
            .run(&[
                "run",
                "-d",
                "--name",
                name,
                "--label",
                &ns.label(),
                "--network",
                net,
                "--entrypoint",
                "sleep",
                &image,
                "300",
            ])
            .unwrap();
    }
    // Squid and Reposilite take a few seconds to start; a fetch before the proxy answers
    // would fail.
    let pom = "http://mirror:8080/central/junit/junit/4.13.2/junit-4.13.2.pom";
    let deadline = Instant::now() + Duration::from_secs(60);
    while (status(
        &inside,
        "https://repo.maven.apache.org/maven2/",
        &["-x", "http://proxy:3128"],
    ) != "200 200"
        || status(&client, "http://mirror:8080/", &["--noproxy", "*"]).ends_with("000"))
        && Instant::now() < deadline
    {
        sleep(Duration::from_secs(1));
    }

    let checks = [
        (
            "an artifact through the mirror",
            status(&client, pom, &["--noproxy", "*"]),
            "000 200",
        ),
        (
            "the same through the proxy, as the SDK library sends it",
            status(&client, pom, &["-x", "http://proxy:3128"]),
            "000 200",
        ),
        (
            "a write to the mirror through the proxy",
            status(
                &client,
                pom,
                &["-x", "http://proxy:3128", "-X", "PUT", "-d", "x"],
            ),
            "000 403",
        ),
        (
            "a write to the mirror",
            status(&client, pom, &["--noproxy", "*", "-X", "PUT", "-d", "x"]),
            "000 401",
        ),
        (
            "Central from the pod",
            status(
                &client,
                "https://repo.maven.apache.org/maven2/",
                &["-x", "http://proxy:3128"],
            ),
            "403 000",
        ),
        (
            "other plain HTTP through the proxy",
            status(&client, "http://example.com/", &["-x", "http://proxy:3128"]),
            "000 403",
        ),
        (
            "Central from the mirror's network",
            status(
                &inside,
                "https://repo.maven.apache.org/maven2/",
                &["-x", "http://proxy:3128"],
            ),
            "200 200",
        ),
        (
            "another host from the mirror's network",
            status(
                &inside,
                "https://example.com/",
                &["-x", "http://proxy:3128"],
            ),
            "403 000",
        ),
        (
            "the internet without the proxy",
            status(
                &inside,
                "https://repo.maven.apache.org/maven2/",
                &["--noproxy", "*", "--max-time", "8"],
            ),
            "000 000",
        ),
    ];

    let logs = Command::new("podman")
        .args(["logs", "--tail", "30", &container])
        .output()
        .unwrap();
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&logs.stdout),
        String::from_utf8_lossy(&logs.stderr)
    );
    // KEEP=1 leaves everything running, to look into a failure.
    if std::env::var_os("KEEP").is_some() {
        return;
    }
    ns.remove_all(&podman, &[]);
    let _ = fs::remove_dir_all(&state_dir);
    let failures: Vec<String> = checks
        .iter()
        .filter(|(_, actual, expected)| actual != expected)
        .map(|(name, actual, expected)| format!("{name}: {actual}, expected {expected}"))
        .collect();
    assert!(
        failures.is_empty(),
        "{}\nmirror log:\n{logs}",
        failures.join("\n")
    );
}
