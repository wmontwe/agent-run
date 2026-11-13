//! The entrypoint of every agent-run container. It starts in place of the agent or build command and
//! holds it until the launcher has compared the container's mounts with the plan:
//!
//! ```text
//! agent-run-gate run -- COMMAND...   wait for the release, then exec COMMAND
//! agent-run-gate attach -- COMMAND... the same, announcing on standard output that it waits
//! agent-run-gate report TARGET...    print the mount table and each target's identity
//! agent-run-gate release             let `run` continue
//! ```
//!
//! The report format is parsed by the `mountplan` crate:
//! `mount <mountinfo line>` per mount, then `target <index> <dev> <ino>` per target
//! (`- -` when a target doesn't exist). No dependencies, so it builds as one static binary.

use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, exit};
use std::thread::sleep;
use std::time::Duration;

/// Nothing but the gate runs before the release, so nothing else can create this.
const RELEASE_DIR: &str = "/tmp/agent-run-gate";
const RELEASED: &str = "/tmp/agent-run-gate/released";

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..], false),
        Some("attach") => run(&args[1..], true),
        Some("report") => report(&args[1..]),
        Some("release") => release(),
        _ => fail(
            "usage: agent-run-gate run|attach -- COMMAND... | report TARGET... | release",
            2,
        ),
    }
}

fn fail(message: &str, code: i32) -> ! {
    eprintln!("agent-run-gate: {message}");
    exit(code);
}

/// What `attach` prints while it waits: an APC sequence, which terminals ignore. The launcher
/// releases the command once it reads it through `podman attach` (then the terminal is
/// connected and misses nothing), and strips it. Must match the launcher's `ANNOUNCE`.
const ANNOUNCE: &[u8] = b"\x1b_agent-run-gate\x1b\\";

fn run(args: &[String], announce: bool) -> ! {
    let [dashes, command @ ..] = args else {
        fail("usage: agent-run-gate run -- COMMAND...", 2)
    };
    if dashes != "--" || command.is_empty() {
        fail("usage: agent-run-gate run -- COMMAND...", 2);
    }
    let mut waited = 0;
    while !Path::new(RELEASED).exists() {
        if announce && waited % 5 == 0 {
            let mut out = std::io::stdout();
            let _ = out.write_all(ANNOUNCE).and_then(|()| out.flush());
        }
        waited += 1;
        sleep(Duration::from_millis(20));
    }
    let error = Command::new(&command[0]).args(&command[1..]).exec();
    fail(&format!("could not start {}: {error}", command[0]), 127)
}

fn report(targets: &[String]) {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .unwrap_or_else(|e| fail(&format!("cannot read mountinfo: {e}"), 1));
    let mut out = String::with_capacity(mountinfo.len() * 2);
    for line in mountinfo.lines() {
        out.push_str("mount ");
        out.push_str(line);
        out.push('\n');
    }
    for (index, target) in targets.iter().enumerate() {
        match fs::symlink_metadata(target) {
            Ok(meta) => out.push_str(&format!("target {index} {} {}\n", meta.dev(), meta.ino())),
            Err(_) => out.push_str(&format!("target {index} - -\n")),
        }
    }
    print!("{out}");
}

fn release() {
    if let Err(e) = fs::create_dir_all(RELEASE_DIR).and_then(|()| fs::write(RELEASED, b"")) {
        fail(&format!("cannot release: {e}"), 1);
    }
}
