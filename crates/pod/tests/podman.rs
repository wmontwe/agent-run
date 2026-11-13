//! The pod lifecycle against real Podman. Needs the Podman machine and an image with
//! `mkdir` (`AGENT_RUN_TEST_IMAGE`, default `localhost/agent-run-claude`).
//! Run with `cargo test -p pod -- --ignored`.

use mountplan::Plan;
use pod::{PodError, Project, Ready, Setup, ensure, remove, remove_if_idle, remove_network};
use podman::{Object, Podman};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use workspace::Rules;

/// Rootless Podman can fail containers with keep-id that start at the same moment
/// (`crun: fchownat ...` or `mount devpts ...: Invalid argument`), so tests take turns.
fn turn() -> std::sync::MutexGuard<'static, ()> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

fn unique(prefix: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn image() -> String {
    std::env::var("AGENT_RUN_TEST_IMAGE")
        .unwrap_or_else(|_| "localhost/agent-run-claude".to_owned())
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

fn checkout() -> PathBuf {
    let root = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(unique("pod-test"))
        .join("checkout");
    for dir in [".git", "gradle/wrapper", "app/build"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("settings.gradle.kts"), "").unwrap();
    root
}

fn fresh_plan(root: &Path, project: &Project) -> Plan {
    let (mask, read_only) = (strings(&[".idea"]), strings(&[".git", "gradle/wrapper"]));
    let inspection = workspace::inspect(
        root,
        &Rules {
            mask: &mask,
            read_only: &read_only,
            private: &[],
        },
    )
    .unwrap();
    mountplan::plan(root, &inspection, &project.state_volume()).unwrap()
}

#[test]
#[ignore = "needs the Podman machine"]
fn creates_reuses_refuses_and_removes_a_pod() {
    let _turn = turn();
    let podman = Podman::default();
    let root = checkout();
    let project = Project::new(&podman::Namespace::new("pod-test"), &root);
    let state_dir = root.parent().unwrap().join("state");
    let image = image();
    let setup = Setup {
        state_dir: &state_dir,
        helper_image: &image,
    };

    // First session: everything is created, and the plan stored.
    let (plan, ready) = ensure(
        &podman,
        &project,
        fresh_plan(&root, &project),
        "rules-1",
        &setup,
    )
    .unwrap();
    assert_eq!(ready, Ready::Created);
    assert!(podman.exists(Object::Pod, &project.pod()).unwrap());
    assert!(podman.exists(Object::Network, &project.network()).unwrap());
    assert!(
        podman
            .exists(Object::Volume, &project.state_volume())
            .unwrap()
    );
    let labels = podman.labels(Object::Pod, &project.pod()).unwrap();
    assert_eq!(labels["agent-run.plan-hash"], plan.hash());
    assert!(
        podman
            .run(&[
                "network",
                "inspect",
                "--format",
                "{{.Internal}}",
                &project.network()
            ])
            .unwrap()
            .trim()
            == "true"
    );

    // A later session gets the stored plan; a folder that appeared since stays unprotected.
    fs::create_dir(root.join(".idea")).unwrap();
    let (reused, ready) = ensure(
        &podman,
        &project,
        fresh_plan(&root, &project),
        "rules-1",
        &setup,
    )
    .unwrap();
    assert_eq!(reused, plan);
    assert_eq!(
        ready,
        Ready::Reused {
            unprotected: strings(&[".idea"])
        }
    );

    // Changed rules and a lost plan are refused.
    assert!(matches!(
        ensure(
            &podman,
            &project,
            fresh_plan(&root, &project),
            "rules-2",
            &setup
        ),
        Err(PodError::RulesChanged)
    ));
    fs::write(
        state_dir.join(format!("{}.plan", project.id)),
        "checkout\t/elsewhere\nvolume\tv\n",
    )
    .unwrap();
    assert!(matches!(
        ensure(
            &podman,
            &project,
            fresh_plan(&root, &project),
            "rules-1",
            &setup
        ),
        Err(PodError::PlanLost)
    ));

    // Without sessions the pod goes; its volume stays for the next one.
    assert!(remove_if_idle(&podman, &project, &setup).unwrap());
    assert!(!podman.exists(Object::Pod, &project.pod()).unwrap());
    assert!(!state_dir.join(format!("{}.plan", project.id)).exists());
    assert!(
        podman
            .exists(Object::Volume, &project.state_volume())
            .unwrap()
    );

    // The next session plans anew, now covering .idea.
    let (replanned, ready) = ensure(
        &podman,
        &project,
        fresh_plan(&root, &project),
        "rules-2",
        &setup,
    )
    .unwrap();
    assert_eq!(ready, Ready::Created);
    assert!(replanned.mounts.iter().any(|m| m.relative == ".idea"));

    remove(&podman, &project, &setup).unwrap();
    podman
        .run(&["volume", "rm", &project.state_volume()])
        .unwrap();
    remove_network(&podman, &project).unwrap();
    assert!(!podman.exists(Object::Network, &project.network()).unwrap());
}

#[test]
#[ignore = "needs the Podman machine"]
fn keeps_a_pod_with_a_running_session() {
    let _turn = turn();
    let podman = Podman::default();
    let root = checkout();
    let project = Project::new(&podman::Namespace::new("pod-test"), &root);
    let state_dir = root.parent().unwrap().join("state");
    let image = image();
    let setup = Setup {
        state_dir: &state_dir,
        helper_image: &image,
    };
    ensure(
        &podman,
        &project,
        fresh_plan(&root, &project),
        "rules",
        &setup,
    )
    .unwrap();
    podman
        .run(&[
            "run",
            "-d",
            "--pod",
            &project.pod(),
            "--name",
            &unique("pod-test-session"),
            "--entrypoint",
            "sleep",
            &image,
            "300",
        ])
        .unwrap();

    assert!(!remove_if_idle(&podman, &project, &setup).unwrap());

    remove(&podman, &project, &setup).unwrap();
    podman
        .run(&["volume", "rm", &project.state_volume()])
        .unwrap();
    remove_network(&podman, &project).unwrap();
    assert!(!podman.exists(Object::Network, &project.network()).unwrap());
}
