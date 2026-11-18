//! Cargo's backend. Toolchains belong to the image; writable caches and outputs belong
//! to the project, never the host's Cargo home or target folders.

use std::path::Path;
use std::process::Command;

pub fn configure(command: &mut Command, workspace: &Path, state: &Path, cwd: &Path) {
    // cwd was canonicalized and checked against workspace before choosing this backend.
    let relative = cwd.strip_prefix(workspace).expect("validated build cwd");
    command
        .env("CARGO_HOME", state.join("rust/cargo"))
        .env(
            "CARGO_TARGET_DIR",
            state.join("rust/targets").join(relative),
        )
        .env("RUSTUP_HOME", "/opt/rustup")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .env_remove("RUSTUP_TOOLCHAIN");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    #[test]
    fn rust_uses_project_storage_and_does_not_inject_gradle_options() {
        let mut command = Command::new("cargo");
        configure(
            &mut command,
            Path::new("/checkout"),
            Path::new("/build-state"),
            Path::new("/checkout/tools/helper"),
        );
        let env: BTreeMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env[OsStr::new("CARGO_HOME")],
            Some(OsStr::new("/build-state/rust/cargo"))
        );
        assert_eq!(
            env[OsStr::new("CARGO_TARGET_DIR")],
            Some(OsStr::new("/build-state/rust/targets/tools/helper"))
        );
        assert_eq!(
            env[OsStr::new("RUSTUP_HOME")],
            Some(OsStr::new("/opt/rustup"))
        );
        assert_eq!(
            env[OsStr::new("RUSTUP_AUTO_INSTALL")],
            Some(OsStr::new("0"))
        );
        assert_eq!(env[OsStr::new("RUSTUP_TOOLCHAIN")], None);
        assert_eq!(command.get_args().count(), 0);
        assert!(!env.contains_key(OsStr::new("GRADLE_USER_HOME")));
    }
}
