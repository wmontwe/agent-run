//! Protection tests against a real session: a throwaway checkout and agent-run home,
//! the session started through the same
//! code as `agent claude` but running `sleep`, probed from outside as the agent's user.
//! Needs the Podman machine and the internet (the GitHub relay); builds the images if needed.
//! Run with `cargo test -p agent --test protection -- --ignored`.

mod common;

use agent::session;
use common::write;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A checkout with everything the rules and the Gradle layout cover.
fn project() -> PathBuf {
    let root = common::checkout("protection");
    write(&root.join(".signing/release.properties"), "secret");
    write(
        &root.join(".idea/workspace.xml"),
        r#"<project><component name="AutoImportSettings"><option name="autoReloadType" value="NONE" /></component></project>"#,
    );
    write(
        &root.join("gradle/wrapper/gradle-wrapper.properties"),
        "official",
    );
    write(&root.join("gradle/libs.versions.toml"), "[versions]\n");
    write(&root.join("settings.gradle.kts"), "");
    write(&root.join("app/src/A.kt"), "class A");
    write(&root.join("app/build/outputs/app.apk"), "host build output");
    write(&root.join("kotlin-js-store/yarn.lock"), "host lock");
    write(&root.join(".claude/settings.json"), "{}");
    write(
        &root.join("local.properties"),
        &format!(
            "sdk.dir={}/Library/Android/sdk\n",
            std::env::var("HOME").unwrap()
        ),
    );
    root
}

/// Whether `script` succeeds in the session's container, in the checkout, as the agent.
fn agent_can(container: &str, checkout: &Path, script: &str) -> bool {
    Command::new("podman")
        .args(["exec", "-w"])
        .arg(checkout)
        .args([container, "sh", "-c", script])
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
#[ignore = "needs the Podman machine"]
fn a_session_protects_what_the_design_says() {
    let root = project();
    let scratch = common::Scratch::new("protection");
    scratch.policy(
        "version = 1\n[network.github]\nrepos = [\"octocat/Hello-World\"]\n[workspace]\nmask = [\".signing\", \".idea\", \".venv\"]\nread-only = [\".git\", \"gradle/wrapper\"]\nprivate = [\"kotlin-js-store\"]\n",
    );
    let paths = scratch.paths();

    let session = session::start(
        "claude",
        &root,
        Some(&["sleep".into(), "600".into()]),
        paths,
    )
    .expect("session starts");
    let container = session.name.clone();
    let pod = session.project.pod();
    let network = session.project.network();

    let refused = [
        ("write .git/config", "echo x >> .git/config"),
        ("write .GIT/config (case alias)", "echo x >> .GIT/config"),
        (
            "add a hook through .gIT/hooks",
            "echo x > .gIT/hooks/pre-commit",
        ),
        ("read .signing", "cat .signing/release.properties"),
        (
            "read .SIGNING (case alias)",
            "cat .SIGNING/release.properties",
        ),
        ("read .idea", "cat .idea/workspace.xml"),
        ("read local.properties", "grep -q sdk.dir local.properties"),
        (
            "write the project's .claude",
            "echo x > .claude/settings.json",
        ),
        (
            "add a hook through .CLAUDE",
            "mkdir -p .CLAUDE/hooks && echo x > .CLAUDE/hooks/planted",
        ),
        (
            "write gradle/wrapper",
            "echo x > gradle/wrapper/gradle-wrapper.properties",
        ),
        ("rename the pinned parent gradle", "mv gradle gradle2"),
        ("rename the pinned parent app", "mv app app2"),
        ("rename the checkout", "mv \"$PWD\" \"$PWD.moved\""),
        ("hard link out of .git", "ln .git/config linked-config"),
        (
            "read the host's build output",
            "cat app/build/outputs/app.apk",
        ),
        (
            "read the host's build output through app/BUILD",
            "cat app/BUILD/outputs/app.apk",
        ),
        (
            "write the host's build folder",
            "echo x > app/build/planted",
        ),
        (
            "read an unlisted repository through the relay",
            "curl -sf \"$GITHUB_API_URL/repos/octocat/Spoon-Knife\"",
        ),
        (
            "write through the relay",
            "curl -sf -X POST -d '{}' \"$GITHUB_API_URL/repos/octocat/Hello-World/issues\"",
        ),
        (
            "reach GitHub without the relay",
            "curl -sf https://api.github.com/repos/octocat/Hello-World",
        ),
    ];
    let allowed = [
        ("write a source file", "echo 'class B' > app/src/B.kt"),
        (
            "edit the version catalog",
            "echo '# edited' >> gradle/libs.versions.toml",
        ),
        (
            "write the root build folder (private)",
            "mkdir -p build && echo x > build/from-agent",
        ),
        (
            "write kotlin-js-store (private)",
            "echo agent > kotlin-js-store/yarn.lock",
        ),
        (
            "read a listed repository through the relay",
            "curl -sf -o /dev/null \"$GITHUB_API_URL/repos/octocat/Hello-World\"",
        ),
    ];
    let mut failures = Vec::new();
    // The build container runs what agents write (build scripts), so it gets the same
    // protection; the GitHub probes don't apply there.
    let build = session.project.build_container();
    for (name, script) in refused {
        if agent_can(&container, &root, script) {
            failures.push(format!("allowed, should be refused: {name}"));
        }
        if !script.contains("GITHUB_API_URL")
            && !name.contains("GitHub")
            && agent_can(&build, &root, script)
        {
            failures.push(format!(
                "allowed in the build container, should be refused: {name}"
            ));
        }
    }
    for (name, script) in allowed {
        if !agent_can(&container, &root, script) {
            failures.push(format!("refused, should be allowed: {name}"));
        }
    }
    for (name, can, script) in [
        (
            "agents write the build state",
            false,
            (&container, "touch /build-state/from-agent"),
        ),
        (
            "the build container writes the build state",
            true,
            (&build, "touch /build-state/from-build"),
        ),
        (
            "agents reach the build server",
            true,
            (&container, "gradle-run 2>&1 | grep -q 'no gradlew'"),
        ),
    ] {
        if agent_can(script.0, &root, script.1) != can {
            failures.push(format!(
                "{}: {name}",
                if can {
                    "refused, should be allowed"
                } else {
                    "allowed, should be refused"
                }
            ));
        }
    }
    // The policy's CPU limits (defaults: 2 per agent session, 8 for the build container).
    for (who, name, expected) in [
        ("agent", &container, "2000000000"),
        ("build", &build, "8000000000"),
    ] {
        let cpus = Command::new("podman")
            .args(["inspect", "--format", "{{.HostConfig.NanoCpus}}", name])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&cpus.stdout).trim() != expected {
            failures.push(format!(
                "{who} container's CPU limit: {}",
                String::from_utf8_lossy(&cpus.stdout).trim()
            ));
        }
    }
    session.end();

    // On the host: protected files unchanged, private writes kept in the container,
    // nothing created for absent rules, the pod gone.
    let host = |relative: &str| fs::read_to_string(root.join(relative)).ok();
    let checks = [
        (
            "gradle-wrapper.properties unchanged",
            host("gradle/wrapper/gradle-wrapper.properties").as_deref() == Some("official"),
        ),
        (
            "signing secret unchanged",
            host(".signing/release.properties").as_deref() == Some("secret"),
        ),
        (
            "host build output unchanged",
            host("app/build/outputs/app.apk").as_deref() == Some("host build output"),
        ),
        (
            "nothing planted in the host build folder",
            host("app/build/planted").is_none(),
        ),
        (
            "the agent's source file arrived",
            host("app/src/B.kt").as_deref() == Some("class B\n"),
        ),
        (
            "root build write stayed private",
            host("build/from-agent").is_none(),
        ),
        (
            "kotlin-js-store write stayed private",
            host("kotlin-js-store/yarn.lock").as_deref() == Some("host lock"),
        ),
        (
            "no .venv created for its rule",
            !root.join(".venv").exists(),
        ),
        ("no hard link created", host("linked-config").is_none()),
        (
            "the pod is gone",
            !Command::new("podman")
                .args(["pod", "exists", &pod])
                .status()
                .unwrap()
                .success(),
        ),
        (
            "its network is gone",
            !Command::new("podman")
                .args(["network", "exists", &network])
                .status()
                .unwrap()
                .success(),
        ),
    ];
    for (name, ok) in checks {
        if !ok {
            failures.push(format!("on the host: {name}"));
        }
    }

    let _ = fs::remove_dir_all(&root);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
