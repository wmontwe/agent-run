//! Cargo through a real session, with no registry dependencies or Gradle fixture.
//! Run on macOS with rootless Podman: cargo test -p agent --test rust -- --ignored.

mod common;

use agent::session;
use common::write;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Checkout(PathBuf);

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exec(container: &str, cwd: &Path, script: &str) -> Output {
    Command::new("podman")
        .args(["exec", "-w"])
        .arg(cwd)
        .args([
            container,
            "timeout",
            "--kill-after=5",
            "120",
            "sh",
            "-ec",
            script,
        ])
        .output()
        .unwrap()
}

#[test]
#[ignore = "needs macOS and the rootless Podman machine"]
fn cargo_run_builds_offline_and_protects_host_targets() {
    // Scratch drops before checkout: containers are removed before their source directory.
    let root = Checkout(common::checkout("rust"));
    write(
        &root.0.join("Cargo.toml"),
        "[package]\nname = \"agent-run-rust-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    write(
        &root.0.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.99.1\"\n",
    );
    write(
        &root.0.join("src/lib.rs"),
        "pub fn answer() -> u8 {\n    42\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn returns_answer() {\n        assert_eq!(super::answer(), 42);\n    }\n\n    #[test]\n    fn receives_build_proxy_identity() {\n        let proxy = std::env::var(\"HTTPS_PROXY\").unwrap();\n        assert!(proxy.starts_with(\"http://build-\"));\n    }\n}\n",
    );
    write(&root.0.join("Target/host-sentinel"), "host-only");
    write(
        &root.0.join("tools/helper/TARGET/host-sentinel"),
        "nested-host-only",
    );

    let scratch = common::Scratch::new("rust");
    // No crate registry or host cache grants: this project must build offline.
    scratch.policy("version = 1\n");
    let session = session::start(
        "claude",
        &root.0,
        Some(&["sleep".into(), "900".into()]),
        scratch.paths(),
    )
    .expect("session starts");
    let mut failures = Vec::new();
    let checks = [
        (
            "agent has a client, not a compiler",
            "command -v cargo-run && ! command -v cargo && ! command -v rustc && case \"$HTTPS_PROXY\" in http://build-*) exit 1 ;; esac",
        ),
        (
            "offline test, format and Clippy through the build container",
            "cargo-run test --offline && cargo-run fmt --all --check && cargo-run clippy --offline --all-targets -- -D warnings",
        ),
        (
            "agent can read but not write build results",
            "test -d /build-state/rust/targets/debug/deps && ! touch /build-state/rust/agent-write",
        ),
        (
            "host targets are hidden through case aliases",
            "! test -e Target/host-sentinel && ! test -e target/host-sentinel && ! test -e TARGET/host-sentinel && ! test -e tools/helper/target/host-sentinel",
        ),
        (
            "host targets cannot be written through case aliases",
            "! touch Target/agent-write && ! touch target/agent-write && ! touch TARGET/agent-write && ! touch tools/helper/target/agent-write",
        ),
    ];
    for (name, script) in checks {
        let output = exec(&session.name, &root.0, script);
        if !output.status.success() {
            failures.push(format!(
                "{name}: {}\n{}{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    session.end();

    for (target, expected) in [
        ("Target", "host-only"),
        ("tools/helper/TARGET", "nested-host-only"),
    ] {
        if fs::read_to_string(root.0.join(target).join("host-sentinel"))
            .ok()
            .as_deref()
            != Some(expected)
        {
            failures.push(format!("{target}: host sentinel changed"));
        }
        let entries: Vec<_> = fs::read_dir(root.0.join(target))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        if entries.len() != 1 {
            failures.push(format!("{target}: unexpected host output: {entries:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
