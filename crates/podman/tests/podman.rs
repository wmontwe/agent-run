//! The shared-service type and the namespace against real Podman. Needs the Podman machine and
//! an image with `sleep` (`localhost/agent-run-build`). Run with `cargo test -p podman -- --ignored`.

use podman::{Namespace, Object, OnChange, Podman, Service};
use std::fs;

const IMAGE: &str = "localhost/agent-run-build";

fn id(podman: &Podman, container: &str) -> String {
    podman
        .run(&[
            "container",
            "inspect",
            "--format",
            "{{.Id}} {{.State.StartedAt}}",
            container,
        ])
        .unwrap()
        .trim()
        .to_owned()
}

/// Removes the test's namespace when the test ends, also when it fails.
struct Cleanup(Namespace);

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.remove_all(&Podman::default(), &[]);
    }
}

#[test]
#[ignore = "needs the Podman machine"]
fn a_service_starts_once_applies_changes_and_follows_its_configuration() {
    let podman = Podman::default();
    let ns = Namespace::new(&format!("podman-test-{}", std::process::id()));
    let _cleanup = Cleanup(ns.clone());
    let base = std::env::temp_dir().join(ns.name("state"));
    let service = |dir: &str| Service {
        podman: &podman,
        container: ns.name("service"),
        network: ns.name("net"),
        internal: true,
        alias: "svc",
        config_dir: base.join(dir),
        label: ns.label(),
    };
    let first = service("a");
    let options = |s: &Service| {
        vec![
            "-v".to_owned(),
            format!("{}:/config:ro,z", s.config_dir.display()),
            "--entrypoint".into(),
            "sleep".into(),
        ]
    };
    let command = vec!["300".to_owned()];
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        if !ok {
            failures.push(format!("{name}: {detail}"));
        }
    };

    check(
        "writing new files is a change",
        first.write_config(&[("x.conf", "1".into())]).unwrap(),
        String::new(),
    );
    first
        .ensure(&options(&first), IMAGE, &command, false, OnChange::Restart)
        .unwrap();
    check(
        "once applied, the same content is no change",
        !first.write_config(&[("x.conf", "1".into())]).unwrap(),
        String::new(),
    );
    let started = id(&podman, &first.container);
    first
        .ensure(&options(&first), IMAGE, &command, false, OnChange::Restart)
        .unwrap();
    check(
        "ensure again changes nothing",
        id(&podman, &first.container) == started,
        String::new(),
    );
    std::thread::sleep(std::time::Duration::from_secs(1));
    first
        .ensure(&options(&first), IMAGE, &command, true, OnChange::Restart)
        .unwrap();
    let restarted = id(&podman, &first.container);
    check(
        "a change restarts the same container",
        restarted != started && restarted.split(' ').next() == started.split(' ').next(),
        format!("{started} -> {restarted}"),
    );

    let other = service("b");
    other.write_config(&[("x.conf", "2".into())]).unwrap();
    other
        .ensure(&options(&other), IMAGE, &command, false, OnChange::Restart)
        .unwrap();
    let replaced = id(&podman, &other.container);
    check(
        "another configuration folder replaces the container",
        replaced.split(' ').next() != restarted.split(' ').next(),
        String::new(),
    );

    let pod_net = ns.name("pod");
    podman
        .run(&[
            "network",
            "create",
            "--internal",
            "--label",
            &ns.label(),
            &pod_net,
        ])
        .unwrap();
    other.connect(&pod_net).unwrap();
    other.connect(&pod_net).unwrap();
    let address = other.address_on(&pod_net).unwrap();
    check(
        "joins a pod's network",
        address.split('.').count() == 4,
        address,
    );
    other.disconnect(&pod_net).unwrap();
    check(
        "leaves it again",
        other.address_on(&pod_net).is_err(),
        String::new(),
    );

    let kept = ns.name("kept");
    let dropped = ns.name("dropped");
    for volume in [&kept, &dropped] {
        podman
            .run(&["volume", "create", "--label", &ns.label(), volume])
            .unwrap();
    }
    ns.remove_all(&podman, &["-kept"]);
    check(
        "remove_all removes containers",
        !podman.exists(Object::Container, &other.container).unwrap(),
        String::new(),
    );
    check(
        "and networks",
        !podman.exists(Object::Network, &pod_net).unwrap(),
        String::new(),
    );
    check(
        "and volumes",
        !podman.exists(Object::Volume, &dropped).unwrap(),
        String::new(),
    );
    check(
        "except kept ones",
        podman.exists(Object::Volume, &kept).unwrap(),
        String::new(),
    );
    let _ = podman.run(&["volume", "rm", &kept]);
    let _ = fs::remove_dir_all(&base);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "needs the Podman machine"]
fn a_rebuilt_image_is_deployed_and_a_failed_reload_is_retried() {
    let podman = Podman::default();
    let ns = Namespace::new(&format!("podman-image-{}", std::process::id()));
    let _cleanup = Cleanup(ns.clone());
    let base = std::env::temp_dir().join(ns.name("state"));
    let image = format!("localhost/{}:latest", ns.name("img"));
    podman.run(&["tag", IMAGE, &image]).unwrap();
    let service = Service {
        podman: &podman,
        container: ns.name("service"),
        network: ns.name("net"),
        internal: true,
        alias: "svc",
        config_dir: base.join("config"),
        label: ns.label(),
    };
    fs::create_dir_all(&service.config_dir).unwrap();
    let data = format!("{}:/data", ns.name("data"));
    let options = vec![
        "-v".to_owned(),
        format!("{}:/config:ro,z", service.config_dir.display()),
        "-v".into(),
        data,
        "--entrypoint".into(),
        "sleep".into(),
    ];
    let command = vec!["300".to_owned()];
    // A reload that fails until /tmp/ok exists in the container, counting its attempts.
    let reload: &[&str] = &["sh", "-c", "echo x >> /tmp/count; test -f /tmp/ok"];
    let count = || {
        podman
            .run(&[
                "exec",
                &service.container,
                "sh",
                "-c",
                "wc -l < /tmp/count 2>/dev/null || echo 0",
            ])
            .unwrap()
            .trim()
            .to_owned()
    };
    let mut failures = Vec::new();

    service.write_config(&[("x.conf", "1".into())]).unwrap();
    service
        .ensure(&options, &image, &command, false, OnChange::Exec(reload))
        .unwrap();
    let changed = service.write_config(&[("x.conf", "2".into())]).unwrap();
    match service.ensure(&options, &image, &command, changed, OnChange::Exec(reload)) {
        Ok(()) => failures.push("the failing reload reported success".to_owned()),
        Err(e)
            if !e.message.contains("exit")
                && !e.message.is_empty()
                && !e.command.starts_with("exec") =>
        {
            failures.push(format!("the reload failed otherwise: {e}"))
        }
        Err(_) => {}
    }
    // The same files again: the reload still has to happen.
    let changed = service.write_config(&[("x.conf", "2".into())]).unwrap();
    let _ = service.ensure(&options, &image, &command, changed, OnChange::Exec(reload));
    if count() != "2" {
        failures.push(format!(
            "a failed reload wasn't retried: {} attempts",
            count()
        ));
    }
    podman
        .run(&["exec", &service.container, "touch", "/tmp/ok"])
        .unwrap();
    let changed = service.write_config(&[("x.conf", "2".into())]).unwrap();
    service
        .ensure(&options, &image, &command, changed, OnChange::Exec(reload))
        .unwrap();
    if service.write_config(&[("x.conf", "2".into())]).unwrap() {
        failures
            .push("after a successful reload, the same files still count as a change".to_owned());
    }

    // Two projects use it, and it keeps data.
    // The projects' networks, its address and alias on each (its own network's may change).
    let networks = || {
        let format = r#"{{range $n, $v := .NetworkSettings.Networks}}{{$n}} {{$v.IPAddress}}{{range $v.Aliases}}{{if eq . "svc"}} svc{{end}}{{end}}{{"\n"}}{{end}}"#;
        let listing = podman
            .run(&[
                "container",
                "inspect",
                "--format",
                format,
                &service.container,
            ])
            .unwrap();
        listing
            .lines()
            .filter(|l| !l.starts_with(&format!("{} ", service.network)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    for project in ["p1", "p2"] {
        podman
            .run(&[
                "network",
                "create",
                "--label",
                &ns.label(),
                &ns.name(project),
            ])
            .unwrap();
        service.connect(&ns.name(project)).unwrap();
    }
    podman
        .run(&[
            "exec",
            &service.container,
            "sh",
            "-c",
            "echo kept > /data/file",
        ])
        .unwrap();
    let attached = networks();

    // The image rebuilt (here: retagged): the running service gets it.
    let image_of = |name: &str| {
        podman
            .run(&["container", "inspect", "--format", "{{.Image}}", name])
            .unwrap()
            .trim()
            .to_owned()
    };
    let before = image_of(&service.container);
    podman
        .run(&["tag", "localhost/agent-run-claude", &image])
        .unwrap();
    if !podman.image_differs(&service.container, &image).unwrap() {
        failures.push("a rebuilt image doesn't count as different".to_owned());
    }
    service
        .ensure(&options, &image, &command, false, OnChange::Exec(reload))
        .unwrap();
    if podman.image_differs(&service.container, &image).unwrap() {
        failures.push("the deployed image counts as different".to_owned());
    }
    if networks() != attached {
        failures.push(format!(
            "the projects' networks before and after the replacement:\n{attached}\n{}",
            networks()
        ));
    }
    if podman
        .run(&["exec", &service.container, "cat", "/data/file"])
        .map(|s| s.trim().to_owned())
        .ok()
        .as_deref()
        != Some("kept")
    {
        failures.push("the service's data was lost".to_owned());
    }
    let wanted = podman
        .run(&["image", "inspect", "--format", "{{.Id}}", &image])
        .unwrap()
        .trim()
        .to_owned();
    if image_of(&service.container) != wanted || before == wanted {
        failures.push(format!(
            "the rebuilt image wasn't deployed: {before} -> {}",
            image_of(&service.container)
        ));
    }
    let _ = podman.run(&["rmi", &image]);
    let _ = fs::remove_dir_all(&base);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
