//! The SDK volume's license synchronization against real Podman.
//! Needs the Podman machine and the build image (`localhost/agent-run-build`).
//! Run with `cargo test -p builds -- --ignored`.

use pod::Project;
use podman::{Namespace, Podman};
use std::fs;
use std::path::Path;

const IMAGE: &str = "localhost/agent-run-build";

struct Cleanup(Namespace);

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.remove_all(&Podman::default(), &[]);
    }
}

fn settings<'a>(image: &'a str, sdk: &Path, record: &'a Path) -> builds::Settings<'a> {
    builds::Settings {
        image,
        memory: 1,
        cpus: 1.0,
        max_concurrent: 1,
        mirror_remotes: vec![],
        mirror_url: "",
        sdk_dir: None,
        host_sdk: Some(sdk.to_path_buf()),
        licenses_copied: record,
    }
}

/// The volume's licenses, as `name=content` lines.
fn in_volume(podman: &Podman, project: &Project) -> String {
    let script = "cd /android/licenses 2>/dev/null && for f in *; do printf '%s=%s\\n' \"$f\" \"$(cat \"$f\")\"; done";
    let volume = format!("{}:/android", project.android_volume());
    podman
        .run(&[
            "run",
            "--rm",
            "-v",
            &volume,
            "--entrypoint",
            "sh",
            IMAGE,
            "-c",
            script,
        ])
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
#[ignore = "needs the Podman machine"]
fn licenses_reach_the_sdk_volume_whenever_they_change_on_the_host() {
    let podman = Podman::default();
    let ns = Namespace::new(&format!("builds-test-{}", std::process::id()));
    let _cleanup = Cleanup(ns.clone());
    let base = std::env::temp_dir().join(ns.name("home"));
    let sdk = base.join("sdk");
    let record = base.join("project.licenses");
    let project = Project::new(&ns, &base.join("app"));
    fs::create_dir_all(&sdk).unwrap();
    project.volume(&podman, &project.android_volume()).unwrap();
    let mut failures = Vec::new();

    // No licenses yet on the host, then one accepted.
    builds::sync_licenses(&podman, &project, &settings(IMAGE, &sdk, &record)).unwrap();
    fs::create_dir_all(sdk.join("licenses")).unwrap();
    fs::write(sdk.join("licenses/android-sdk-license"), "a1\n").unwrap();
    // A failed copy is retried next time.
    if builds::sync_licenses(
        &podman,
        &project,
        &settings("localhost/no-such-image", &sdk, &record),
    )
    .is_ok()
    {
        failures.push("a copy with a missing image succeeded".to_owned());
    }
    builds::sync_licenses(&podman, &project, &settings(IMAGE, &sdk, &record)).unwrap();
    if in_volume(&podman, &project) != "android-sdk-license=a1" {
        failures.push(format!(
            "after the first license: {:?}",
            in_volume(&podman, &project)
        ));
    }
    // Another license, and a changed one.
    fs::write(sdk.join("licenses/android-sdk-preview-license"), "p1\n").unwrap();
    fs::write(sdk.join("licenses/android-sdk-license"), "a1\na2\n").unwrap();
    builds::sync_licenses(&podman, &project, &settings(IMAGE, &sdk, &record)).unwrap();
    let expected = "android-sdk-license=a1\na2\nandroid-sdk-preview-license=p1";
    if in_volume(&podman, &project) != expected {
        failures.push(format!(
            "after a change: {:?}",
            in_volume(&podman, &project)
        ));
    }
    if fs::read_to_string(sdk.join("licenses/android-sdk-license")).unwrap() != "a1\na2\n" {
        failures.push("the host's license changed".to_owned());
    }
    let _ = fs::remove_dir_all(&base);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
