//! Kotlin Toolchain through `kotlin-run` in a real session: a minimal JVM app with one
//! dependency from Central, built and run from the agent's container. Every download goes
//! through the mirror over HTTPS (Kotlin Toolchain can't use the proxy). Needs the Podman
//! machine, the internet, and a project wrapper pinning Kotlin Toolchain 0.14.0 or newer to
//! copy (`AGENT_RUN_TEST_KOTLIN_WRAPPER_FROM`, the path of a `kotlin` script). The mirror's
//! volume is kept between runs, so the distribution downloads once.
//! Run with `cargo test -p agent --test kotlin -- --ignored`.

mod common;

use agent::session;
use common::write;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A JVM app that needs kotlinx-coroutines from Central.
fn project() -> PathBuf {
    let from = PathBuf::from(
        std::env::var("AGENT_RUN_TEST_KOTLIN_WRAPPER_FROM")
            .expect("AGENT_RUN_TEST_KOTLIN_WRAPPER_FROM: a 0.14.0 or newer `kotlin` wrapper"),
    );
    let root = common::checkout("kotlin");
    fs::copy(&from, root.join("kotlin")).unwrap();
    fs::set_permissions(root.join("kotlin"), fs::Permissions::from_mode(0o755)).unwrap();
    write(
        &root.join("module.yaml"),
        "product: jvm/app\n\ndependencies:\n  - org.jetbrains.kotlinx:kotlinx-coroutines-core:1.11.0\n",
    );
    write(
        &root.join("src/main.kt"),
        "import kotlinx.coroutines.runBlocking\n\nfun main() = runBlocking { println(\"tiny ok\") }\n",
    );
    root
}

fn exec(container: &str, cwd: &Path, script: &str) -> Output {
    Command::new("podman")
        .args(["exec", "-w"])
        .arg(cwd)
        .args([container, "sh", "-c", script])
        .output()
        .unwrap()
}

#[test]
#[ignore = "needs the Podman machine and the internet"]
fn kotlin_run_builds_and_runs_through_the_https_mirror() {
    let root = project();
    // A fixed namespace keeps the mirror's volume between runs: the distribution downloads once.
    let mut scratch = common::Scratch::fixed("kotlin");
    scratch.keep.push("mirror-data");
    scratch.policy(
        "version = 1\n[mirror.remotes]\ncentral = \"https://repo.maven.apache.org/maven2\"\n\
         kotlin-toolchain = \"https://artifacts-caching-proxy.aws.intellij.net/packages.jetbrains.team/maven/p/amper/amper\"\n\
         jetbrains-download = \"https://download.jetbrains.com\"\n",
    );
    let paths = scratch.paths();
    let mirror = scratch.namespace.name("mirror");

    let session = session::start(
        "claude",
        &root,
        Some(&["sleep".into(), "1800".into()]),
        paths,
    )
    .expect("session starts");
    let agent = session.name.clone();
    let mut failures = Vec::new();

    let run = exec(&agent, &root, "kotlin-run run");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    if !run.status.success() || !output.contains("tiny ok") {
        failures.push(format!("the app didn't build and run:\n{output}"));
    }
    let checks = [
        (
            "the distribution came through the mirror",
            exec(
                &mirror,
                Path::new("/"),
                "ls /app/data/repositories/kotlin-toolchain/org/jetbrains/kotlin/kotlin-cli/*/*-dist.tgz",
            )
            .status
            .success(),
        ),
        (
            "the dependency came through the mirror",
            exec(
                &mirror,
                Path::new("/"),
                "test -d /app/data/repositories/central/org/jetbrains/kotlinx/kotlinx-coroutines-core/1.11.0",
            )
            .status
            .success(),
        ),
    ];
    for (name, ok) in checks {
        if !ok {
            failures.push(name.to_owned());
        }
    }
    // Missing remotes are named, not left to an unknown host.
    if !output.contains("no mirror remote for https://nodejs.org/dist") {
        failures.push(format!("missing remotes weren't reported:\n{output}"));
    }
    // Nothing of the build in the checkout but the safety net's mount point.
    if fs::read_dir(root.join("build"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
        failures.push("files in the checkout's build folder".into());
    }

    session.end();
    let _ = fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
