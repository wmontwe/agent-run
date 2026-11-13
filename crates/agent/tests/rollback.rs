//! A start that fails after the pod exists: the pod and everything of
//! it goes again, unless another session runs in it, which stays untouched. Failures are
//! injected after each step (`AGENT_RUN_FAIL_AFTER`), in this test binary only.
//! Run with `cargo test -p agent --test rollback -- --ignored`.

mod common;

use agent::session;
use std::process::Command;

fn exists(kind: &str, name: &str) -> bool {
    Command::new("podman")
        .args([kind, "exists", name])
        .status()
        .unwrap()
        .success()
}

/// What a failed start may leave in the scratch home: proxy credentials, session files.
fn files_left(scratch: &common::Scratch, keep: usize) -> Vec<String> {
    let mut left = Vec::new();
    for dir in ["state/proxy-users", "state/sessions"] {
        let count = std::fs::read_dir(scratch.base.join(dir))
            .map(|d| d.count())
            .unwrap_or(0);
        if count > keep {
            left.push(format!("{dir}: {count} entries"));
        }
    }
    left
}

/// The Xvfb processes in the build container, by display.
fn displays(build: &str) -> String {
    let script = "for p in /proc/[0-9]*; do tr '\\0' ' ' < $p/cmdline 2>/dev/null; echo; done | grep '^Xvfb' | sort";
    let out = Command::new("podman")
        .args(["exec", build, "sh", "-c", script])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
#[ignore = "needs the Podman machine"]
fn a_failed_start_leaves_nothing_and_touches_no_running_session() {
    let root = common::checkout("rollback");
    let scratch = common::Scratch::new("rollback");
    scratch.policy("version = 1\n");
    let sleep = ["sleep".to_owned(), "600".to_owned()];
    let mut failures = Vec::new();
    for step in [
        "pod",
        "services",
        "build container",
        "display and browser",
        "container",
    ] {
        // Safety: this test binary has this one test; nothing reads the environment concurrently.
        unsafe { std::env::set_var("AGENT_RUN_FAIL_AFTER", step) };
        let failed = session::start("claude", &root, Some(&sleep), scratch.paths());
        unsafe { std::env::remove_var("AGENT_RUN_FAIL_AFTER") };
        let project = pod::Project::new(&scratch.namespace, &root);
        if failed.is_ok() {
            failures.push(format!(
                "{step}: the injected failure didn't fail the start"
            ));
        }
        for (kind, name) in [
            ("pod", project.pod()),
            ("container", project.build_container()),
            ("network", project.network()),
        ] {
            if exists(kind, &name) {
                failures.push(format!("after a failure in {step}: {kind} {name} left"));
            }
        }
        failures.extend(
            files_left(&scratch, 0)
                .into_iter()
                .map(|f| format!("after a failure in {step}: {f}")),
        );
    }
    // With a session running, a failed second start leaves it alone.
    let running =
        session::start("claude", &root, Some(&sleep), scratch.paths()).expect("a session");
    let before = displays(&running.project.build_container());
    for step in ["display and browser", "container"] {
        unsafe { std::env::set_var("AGENT_RUN_FAIL_AFTER", step) };
        let _ = session::start("claude", &root, Some(&sleep), scratch.paths());
        unsafe { std::env::remove_var("AGENT_RUN_FAIL_AFTER") };
        failures.extend(
            files_left(&scratch, 1)
                .into_iter()
                .map(|f| format!("beside a session, after a failure in {step}: {f}")),
        );
        let after = displays(&running.project.build_container());
        if after != before {
            failures.push(format!(
                "beside a session, after a failure in {step}: displays {after:?}, before {before:?}"
            ));
        }
    }
    for (kind, name) in [
        ("pod", running.project.pod()),
        ("container", running.name.clone()),
        ("container", running.project.build_container()),
    ] {
        if !exists(kind, &name) {
            failures.push(format!("the running session lost its {kind} {name}"));
        }
    }
    running.end();
    let _ = std::fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
