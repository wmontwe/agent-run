//! The `bin/agent` wrapper: it builds the launcher
//! with Cargo's configuration from agent-run's repository only, then runs it in the caller's
//! folder. A checkout's `.cargo/config.toml`, which agents can write, must never take effect: it
//! could run code on the host. The wrapper runs from a temporary installation; a stand-in
//! `cargo` records where it runs and with what, and emits a stand-in launcher that records its
//! folder and arguments. Nothing is built: the real launcher is tested through
//! `CARGO_BIN_EXE_agent` (tests/cli.rs).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Installation {
    base: PathBuf,
    /// Where the wrapper is installed: `bin/agent` in it.
    root: PathBuf,
    stubs: PathBuf,
}

impl Installation {
    /// The wrapper in a fresh installation; `cargo` exits with `cargo_status` after emitting a
    /// launcher that exits with `launcher_status`.
    fn new(name: &str, cargo_status: i32, launcher_status: i32) -> Installation {
        let base = std::env::temp_dir().join(format!("wrapper-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (root, stubs) = (base.join("install"), base.join("stubs"));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(&stubs).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let repository = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        fs::copy(repository.join("bin/agent"), root.join("bin/agent")).unwrap();
        let launcher = format!(
            "#!/bin/sh\n{{ pwd -P; printf '%s\\n' \"$@\"; }} > {}\nexit {launcher_status}\n",
            base.join("launcher-call").display()
        );
        let cargo = format!(
            "#!/bin/sh\n{{ pwd -P; printf '%s\\n' \"$@\"; }} > {}\nmkdir -p target/release\ncat > target/release/agent <<'EOF'\n{launcher}EOF\nchmod +x target/release/agent\nexit {cargo_status}\n",
            base.join("cargo-call").display()
        );
        fs::write(stubs.join("cargo"), cargo).unwrap();
        fs::set_permissions(stubs.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
        Installation { base, root, stubs }
    }

    fn run(&self, wrapper: &Path, caller: &Path, args: &[&str]) -> std::process::ExitStatus {
        Command::new(wrapper)
            .args(args)
            .current_dir(caller)
            .env("PATH", format!("{}:/usr/bin:/bin", self.stubs.display()))
            .status()
            .unwrap()
    }

    /// The folder a stand-in ran in, and its arguments.
    fn call(&self, who: &str) -> (PathBuf, Vec<String>) {
        let text = fs::read_to_string(self.base.join(format!("{who}-call"))).unwrap_or_default();
        let mut lines = text.lines();
        (
            PathBuf::from(lines.next().unwrap_or_default()),
            lines.map(str::to_owned).collect(),
        )
    }
}

impl Drop for Installation {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn the_wrapper_builds_in_its_installation_and_runs_the_launcher_in_the_callers_folder() {
    let install = Installation::new("run", 0, 7);
    let caller = install.base.join("checkout");
    fs::create_dir_all(caller.join(".cargo")).unwrap();
    fs::write(
        caller.join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = 123\n",
    )
    .unwrap();
    let caller = fs::canonicalize(caller).unwrap();

    let status = install.run(
        &install.root.join("bin/agent"),
        &caller,
        &["claude", "a b", "", "c"],
    );
    let (cargo_folder, cargo_args) = install.call("cargo");
    assert_eq!(
        cargo_folder, install.root,
        "cargo ran outside the installation"
    );
    assert!(cargo_args.iter().any(|a| a == "--locked"), "{cargo_args:?}");
    let (launcher_folder, launcher_args) = install.call("launcher");
    assert_eq!(
        launcher_folder, caller,
        "the launcher ran outside the caller's folder"
    );
    assert_eq!(launcher_args, ["claude", "a b", "", "c"], "arguments");
    assert_eq!(status.code(), Some(7), "the launcher's exit status");
}

#[test]
fn a_failed_build_runs_no_launcher() {
    let install = Installation::new("fail", 3, 0);
    let status = install.run(&install.root.join("bin/agent"), &install.base, &["claude"]);
    assert_ne!(status.code(), Some(0));
    assert!(
        !install.base.join("launcher-call").exists(),
        "a launcher ran after the build failed"
    );
}

#[test]
fn the_wrapper_finds_its_installation_through_symlinks() {
    let install = Installation::new("links", 0, 0);
    let links = install.base.join("links");
    fs::create_dir_all(links.join("nested")).unwrap();
    // Absolute, relative (to a relative link), and chained.
    std::os::unix::fs::symlink(install.root.join("bin/agent"), links.join("absolute")).unwrap();
    std::os::unix::fs::symlink("../absolute", links.join("nested/relative")).unwrap();
    std::os::unix::fs::symlink(links.join("nested/relative"), links.join("chained")).unwrap();
    for link in ["absolute", "nested/relative", "chained"] {
        let _ = fs::remove_file(install.base.join("cargo-call"));
        install.run(&links.join(link), &install.base, &["claude"]);
        assert_eq!(install.call("cargo").0, install.root, "through {link}");
    }
}
