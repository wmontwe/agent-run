//! The gate and mount verification against real containers.
//! Needs the Podman machine, `localhost/agent-run-gate` (see
//! crates/gate/Containerfile) and an agent image (`AGENT_RUN_TEST_IMAGE`, default
//! `localhost/agent-run-claude`). Run with `cargo test -p mountplan -- --ignored`.

use mountplan::{Plan, parse_report, plan, verify};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};
use workspace::Rules;

const GATE: &str = "/usr/local/bin/agent-run-gate";

fn podman(args: &[String]) -> Result<String, String> {
    let output = Command::new("podman")
        .args(args)
        .output()
        .map_err(|e| format!("podman: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

fn image() -> String {
    std::env::var("AGENT_RUN_TEST_IMAGE")
        .unwrap_or_else(|_| "localhost/agent-run-claude".to_owned())
}

fn unique(prefix: &str) -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The gate binary, copied out of its image once per test run.
fn gate_binary() -> &'static Path {
    static GATE_BINARY: OnceLock<PathBuf> = OnceLock::new();
    GATE_BINARY.get_or_init(|| {
        let dir = std::env::temp_dir().join(unique("agent-run-gate"));
        fs::create_dir_all(&dir).unwrap();
        let name = unique("gate-extract");
        podman(&strings(&[
            "create",
            "--name",
            &name,
            "localhost/agent-run-gate",
            "/agent-run-gate",
        ]))
        .expect("build localhost/agent-run-gate first");
        podman(&strings(&[
            "cp",
            &format!("{name}:/agent-run-gate"),
            &dir.display().to_string(),
        ]))
        .unwrap();
        podman(&strings(&["rm", &name])).unwrap();
        dir.join("agent-run-gate")
    })
}

/// A checkout in the temp folder, which the Podman machine shares.
fn checkout() -> PathBuf {
    let root = fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(unique("mountplan-podman"))
        .join("checkout");
    for dir in [
        ".git/hooks",
        ".signing",
        "gradle/wrapper",
        "app/src",
        "app/build",
        "kotlin-js-store",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("settings.gradle.kts"), "").unwrap();
    fs::write(root.join(".signing/release.jks"), "secret").unwrap();
    fs::write(
        root.join("gradle/wrapper/gradle-wrapper.properties"),
        "official",
    )
    .unwrap();
    root
}

fn plan_for(root: &Path, volume: &str) -> Plan {
    let (mask, read_only, private) = (
        strings(&[".signing"]),
        strings(&[".git", "gradle/wrapper"]),
        strings(&["kotlin-js-store"]),
    );
    let inspection = workspace::inspect(
        root,
        &Rules {
            mask: &mask,
            read_only: &read_only,
            private: &private,
        },
    )
    .unwrap();
    let plan = plan(root, &inspection, volume).unwrap();
    // Podman mounts a volume subpath only if it exists.
    podman(&strings(&["volume", "create", volume])).unwrap();
    let mut args = strings(&[
        "run",
        "--rm",
        "--network",
        "none",
        "--userns",
        "keep-id:uid=1000,gid=1000",
        "-v",
    ]);
    args.extend([
        format!("{volume}:/state"),
        "--workdir".into(),
        "/state".into(),
        "--entrypoint".into(),
        "mkdir".into(),
        image(),
        "-p".into(),
    ]);
    args.extend(plan.subpaths().iter().map(|s| (*s).to_owned()));
    podman(&args).unwrap();
    plan
}

struct Outcome {
    released: bool,
    ran: bool,
    problems: Vec<String>,
}

/// Starts a container behind the gate with `args` (the plan's, possibly tampered with),
/// verifies it against `plan`, and releases it only on a match.
fn run_gated(plan: &Plan, mount_args: Vec<String>) -> Outcome {
    let name = unique("mountplan-gated");
    let mut args = strings(&[
        "run",
        "-d",
        "--name",
        &name,
        "--network",
        "none",
        "--read-only",
        "--cap-drop",
        "ALL",
    ]);
    args.extend(strings(&[
        "--userns",
        "keep-id:uid=1000,gid=1000",
        "--security-opt",
        "no-new-privileges",
    ]));
    args.extend([
        "-v".into(),
        format!("{}:{GATE}:ro", gate_binary().display()),
    ]);
    args.extend(mount_args);
    args.extend([
        "--entrypoint".into(),
        GATE.into(),
        image(),
        "run".into(),
        "--".into(),
    ]);
    args.extend(strings(&[
        "sh",
        "-c",
        "touch /tmp/agent-ran; exec sleep 300",
    ]));
    let started = podman(&args);
    let outcome = match started {
        Err(e) => Outcome {
            released: false,
            ran: false,
            problems: vec![format!("start failed: {e}")],
        },
        Ok(_) => {
            let mut report_args = strings(&["exec", &name, GATE, "report"]);
            report_args.extend(
                plan.mounts
                    .iter()
                    .map(|m| plan.target(m).display().to_string()),
            );
            let problems = match podman(&report_args)
                .and_then(|text| parse_report(&text, plan.mounts.len()))
            {
                Ok(report) => verify(plan, &report),
                Err(e) => vec![format!("no report: {e}")],
            };
            let released = problems.is_empty();
            let mut ran = false;
            if released {
                podman(&strings(&["exec", &name, GATE, "release"])).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                while !ran && Instant::now() < deadline {
                    ran =
                        podman(&strings(&["exec", &name, "test", "-e", "/tmp/agent-ran"])).is_ok();
                    sleep(Duration::from_millis(50));
                }
            } else {
                sleep(Duration::from_millis(300));
                ran = podman(&strings(&["exec", &name, "test", "-e", "/tmp/agent-ran"])).is_ok();
            }
            Outcome {
                released,
                ran,
                problems,
            }
        }
    };
    let _ = podman(&strings(&["rm", "-f", "-t", "0", &name]));
    outcome
}

fn scenario(tamper: impl FnOnce(&Path, &mut Vec<String>)) -> Outcome {
    // Rootless Podman can fail containers with keep-id that start at the same moment
    // (`crun: fchownat ... Invalid argument`), so the scenarios run one at a time.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let root = checkout();
    let volume = unique("mountplan-state");
    let plan = plan_for(&root, &volume);
    let mut args = plan.podman_args();
    tamper(&root, &mut args);
    let outcome = run_gated(&plan, args);
    let _ = podman(&strings(&["volume", "rm", "-f", &volume]));
    outcome
}

fn assert_blocked(outcome: &Outcome, expected: &str) {
    assert!(
        !outcome.released && !outcome.ran,
        "should be blocked: {:?}",
        outcome.problems
    );
    assert!(
        outcome.problems.iter().any(|p| p.contains(expected)),
        "expected {expected:?} in {:?}",
        outcome.problems
    );
}

#[test]
#[ignore = "needs the Podman machine and localhost/agent-run-gate"]
fn releases_a_container_that_matches_its_plan() {
    let outcome = scenario(|_, _| {});
    assert!(outcome.released && outcome.ran, "{:?}", outcome.problems);
}

#[test]
#[ignore = "needs the Podman machine and localhost/agent-run-gate"]
fn blocks_folders_rearranged_after_planning() {
    // What a running container could do: rename the parent, recreate the protected folder.
    let outcome = scenario(|root, _| {
        fs::rename(root.join("gradle"), root.join("gradle2")).unwrap();
        fs::create_dir_all(root.join("gradle/wrapper")).unwrap();
        fs::write(
            root.join("gradle/wrapper/gradle-wrapper.properties"),
            "planted",
        )
        .unwrap();
    });
    assert_blocked(&outcome, "inode differs");
}

#[test]
#[ignore = "needs the Podman machine and localhost/agent-run-gate"]
fn blocks_a_missing_mask() {
    let outcome = scenario(|_, args| {
        let i = args.iter().position(|a| a.starts_with("mask=")).unwrap();
        args.drain(i - 1..=i);
    });
    assert_blocked(&outcome, ".signing not mounted");
}

#[test]
#[ignore = "needs the Podman machine and localhost/agent-run-gate"]
fn blocks_swapped_volume_subpaths() {
    let outcome = scenario(|_, args| {
        for arg in args.iter_mut() {
            if arg.contains("subpath=private/build,") {
                *arg = arg.replace("subpath=private/build,", "subpath=private/kotlin-js-store,");
            } else if arg.contains("subpath=private/kotlin-js-store,") {
                *arg = arg.replace("subpath=private/kotlin-js-store,", "subpath=private/build,");
            }
        }
    });
    assert_blocked(&outcome, "source");
}

#[test]
#[ignore = "needs the Podman machine and localhost/agent-run-gate"]
fn blocks_an_unplanned_mount() {
    let outcome = scenario(|root, args| {
        let src = root.join("app/src").display().to_string();
        args.extend([
            "--mount".into(),
            format!("type=bind,src={src},dst={src},ro"),
        ]);
    });
    assert_blocked(&outcome, "unplanned mount app/src");
}

#[test]
#[ignore = "needs the Podman machine and localhost/agent-run-gate"]
fn blocks_a_protected_folder_mounted_read_write() {
    let outcome = scenario(|_, args| {
        for arg in args.iter_mut() {
            if arg.ends_with("/gradle/wrapper,ro") {
                arg.truncate(arg.len() - ",ro".len());
            }
        }
    });
    assert_blocked(&outcome, "mounted read-write");
}
