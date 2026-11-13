//! Builds through `gradle-run` in a real session: a
//! minimal Java project that needs Central and JDK 17, built from the agent's container. Needs
//! the Podman machine, the internet, and a checkout with a Gradle wrapper to copy `gradlew`
//! and `gradle-wrapper.jar` from (`AGENT_RUN_TEST_WRAPPER_FROM`). The mirror's volume is kept
//! between runs, so the distribution downloads once.
//! Run with `cargo test -p agent --test builds -- --ignored`.

mod common;

use agent::session;
use common::write;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

/// A Java project with one dependency from Central and a JDK 17 toolchain.
fn project() -> PathBuf {
    let from = PathBuf::from(
        std::env::var("AGENT_RUN_TEST_WRAPPER_FROM")
            .expect("AGENT_RUN_TEST_WRAPPER_FROM: a checkout with a Gradle wrapper"),
    );
    let root = common::checkout("builds");
    for file in ["gradlew", "gradle/wrapper/gradle-wrapper.jar"] {
        fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
        fs::copy(from.join(file), root.join(file)).unwrap();
    }
    write(
        &root.join("gradle/wrapper/gradle-wrapper.properties"),
        "distributionBase=GRADLE_USER_HOME\ndistributionPath=wrapper/dists\n\
         distributionUrl=https\\://services.gradle.org/distributions/gradle-9.7.0-bin.zip\n\
         zipStoreBase=GRADLE_USER_HOME\nzipStorePath=wrapper/dists\n",
    );
    write(
        &root.join("settings.gradle.kts"),
        "rootProject.name = \"mini\"\n",
    );
    write(
        &root.join("build.gradle.kts"),
        "plugins { java }\nrepositories { mavenCentral() }\n\
         java { toolchain { languageVersion = JavaLanguageVersion.of(17) } }\n\
         dependencies { implementation(\"junit:junit:4.13.2\") }\n",
    );
    write(
        &root.join("src/main/java/mini/A.java"),
        "package mini;\npublic class A { org.junit.Assert unused; }\n",
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
fn gradle_run_builds_through_the_mirror_with_state_outside_the_checkout() {
    let root = project();
    // A fixed namespace keeps the mirror's volume between runs: the distribution downloads once.
    let mut scratch = common::Scratch::fixed("builds");
    scratch.keep.push("mirror-data");
    scratch.policy(
        "version = 1\n[mirror.remotes]\ncentral = \"https://repo.maven.apache.org/maven2\"\n\
         gradle-distributions = \"https://services.gradle.org/distributions\"\n",
    );
    let paths = scratch.paths();
    let mirror = scratch.namespace.name("mirror");

    let session = session::start(
        "claude",
        &root,
        Some(&["sleep".into(), "900".into()]),
        paths,
    )
    .expect("session starts");
    let agent = session.name.clone();
    let mut failures = Vec::new();

    let started = Instant::now();
    let build = exec(&agent, &root, "gradle-run build --console=plain");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    eprintln!("first build: {} s", started.elapsed().as_secs());
    if !build.status.success() || !output.contains("BUILD SUCCESSFUL") {
        failures.push(format!("the build failed:\n{output}"));
    }

    // Two builds at once: one waits for the other and says so.
    let spawn = || {
        Command::new("podman")
            .args(["exec", "-w"])
            .arg(&root)
            .args([&agent, "gradle-run", "build", "--rerun-tasks"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let (first, second) = (spawn(), spawn());
    let told: String = [first, second]
        .into_iter()
        .map(|c| String::from_utf8_lossy(&c.wait_with_output().unwrap().stderr).into_owned())
        .collect();
    if !told.contains("waiting: 1 build(s) of this project running") {
        failures.push(format!("neither build waited: {told}"));
    }

    let checks = [
        (
            "the agent reads the build's jar",
            exec(
                &agent,
                &root,
                "test -f /build-state/projects/build/libs/mini.jar",
            )
            .status
            .success(),
        ),
        (
            "the dependency came through the mirror",
            exec(
                &mirror,
                Path::new("/"),
                "test -d /app/data/repositories/central/junit/junit/4.13.2",
            )
            .status
            .success(),
        ),
        (
            "the distribution came through the mirror",
            exec(
                &mirror,
                Path::new("/"),
                "test -f /app/data/repositories/gradle-distributions/gradle-9.7.0-bin.zip",
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
    // The root build folder is the private safety net's mount point: it may exist, empty.
    if fs::read_dir(root.join("build"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
        failures.push("files in the checkout's build folder".into());
    }
    for state in [".gradle", ".kotlin"] {
        if root.join(state).exists() {
            failures.push(format!("{state} in the checkout"));
        }
    }

    session.end();
    let _ = fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
