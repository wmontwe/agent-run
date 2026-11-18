//! The client and server binaries end to end, on this machine, with a stand-in build: output,
//! exit status, the queue, cancelling. No Podman.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("buildrun-{name}-{}", std::process::id()));
    fs::create_dir_all(dir.join("checkout/app")).unwrap();
    fs::write(dir.join("checkout/gradlew"), "").unwrap();
    dir.canonicalize().unwrap()
}

/// A stand-in build: prints its arguments and DISPLAY, writes a marker while it runs, then
/// sleeps if asked and exits with the status asked for.
fn stand_in(dir: &Path) -> PathBuf {
    let script = dir.join("build.sh");
    fs::write(
        &script,
        // The version line lets it stand in for a Kotlin Toolchain wrapper too.
        "#!/bin/sh\nkotlin_cli_version=0.14.0\necho \"args: $*\"\necho \"display: ${DISPLAY:-none}\" >&2\ntouch \"$MARKER.$$\"\n\
         [ \"$1\" = slow ] && sleep \"$2\"\n\
         [ \"$1\" = pipes ] && { sleep 30 & echo $! > \"$MARKER.child\"; sleep 30; }\nrm -f \"$MARKER.$$\"\nexit \"${EXIT:-0}\"\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn start_server(dir: &Path, port: u16) -> Server {
    let mut server = server_command(dir, port, "http://mirror:8080", 1);
    server.env("AGENT_RUN_BUILD_COMMAND", stand_in(dir));
    Server::start(server)
}

/// The server without a stand-in: it installs the wrapper's distribution, then runs `gradlew`.
fn server_command(dir: &Path, port: u16, mirror: &str, max: usize) -> Command {
    let mut server = Command::new(env!("CARGO_BIN_EXE_gradle-run-server"));
    server
        .env("AGENT_RUN_BUILD_PORT", port.to_string())
        .env("AGENT_RUN_WORKSPACE", dir.join("checkout"))
        .env("GRADLE_USER_HOME", dir.join("gradle"))
        .env("AGENT_RUN_BUILD_STATE", dir.join("state"))
        .env("AGENT_RUN_MIRROR", mirror)
        .env("AGENT_RUN_JDKS", "/opt/jdk/21")
        .env("AGENT_RUN_INIT_SCRIPTS", "/etc/agent-run")
        .env("AGENT_RUN_MAX_CONCURRENT", max.to_string())
        .env("MARKER", dir.join("running"))
        .env("EXIT", "3")
        .stderr(Stdio::null());
    server
}

impl Server {
    fn start(mut command: Command) -> Server {
        // In production the server is PID 1. Make this standalone Linux fixture adopt
        // its own orphan descendants too, rather than relying on the test host's init.
        #[cfg(target_os = "linux")]
        unsafe {
            command.pre_exec(|| {
                rustix::process::set_child_subreaper(Some(rustix::process::getpid()))
                    .map_err(std::io::Error::from)
            });
        }
        let child = command.spawn().unwrap();
        sleep(Duration::from_millis(300));
        Server(child)
    }
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// The child's status if it exits before the deadline; killed otherwise.
fn finishes(mut child: Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

fn client(dir: &Path, port: u16, args: &[&str]) -> Command {
    tool_client(env!("CARGO_BIN_EXE_gradle-run"), dir, port, args)
}

fn cargo_client(dir: &Path, port: u16, args: &[&str]) -> Command {
    tool_client(env!("CARGO_BIN_EXE_cargo-run"), dir, port, args)
}

fn kotlin_client(dir: &Path, port: u16, args: &[&str]) -> Command {
    tool_client(env!("CARGO_BIN_EXE_kotlin-run"), dir, port, args)
}

fn tool_client(binary: &str, dir: &Path, port: u16, args: &[&str]) -> Command {
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(dir.join("checkout/app"))
        .env("AGENT_RUN_BUILD_PORT", port.to_string())
        .env("DISPLAY", ":7");
    command
}

/// Process ids of the stand-in builds still marked as running.
fn markers(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_string_lossy()
                .strip_prefix("running.")
                .map(String::from)
        })
        .collect()
}

#[test]
fn kotlin_client_runs_the_project_wrapper_with_isolated_state_and_exact_arguments() {
    let dir = scratch("kotlin");
    let root = dir.join("checkout");
    fs::write(root.join("project.yaml"), "modules: [app]\n").unwrap();
    fs::write(root.join("kotlin"), "#!/bin/sh\nkotlin_cli_version=0.14.0\n[ \"${JAVA_HOME-}\" = /opt/jdk/25 ] || exit 91\n[ \"${KOTLIN_CLI_JAVA_HOME-}\" = /opt/jdk/25 ] || exit 92\n[ -z \"${JAVA_TOOL_OPTIONS+x}\" ] || exit 93\n[ \"$KOTLIN_TOOLCHAIN_FUS_ENABLED\" = false ] || exit 94\nif read -r unused; then exit 95; fi\nprintf 'arg=<%s>\\n' \"$@\"\nprintf 'cwd=%s\\nbuild=%s\\ncache=%s\\ndisplay=%s\\n' \"$PWD\" \"$KOTLIN_TOOLCHAIN_BUILD_DIR\" \"$KOTLIN_SHARED_CACHE_DIR\" \"$DISPLAY\"\nexit 7\n").unwrap();
    fs::set_permissions(root.join("kotlin"), fs::Permissions::from_mode(0o755)).unwrap();
    let port = 23300 + (std::process::id() % 1000) as u16;
    let _server = Server::start(server_command(&dir, port, "http://mirror:8080", 1));
    let output = kotlin_client(
        &dir,
        port,
        &[
            "do",
            "customCommand",
            "--module",
            "api",
            "--module",
            "a b",
            "",
            "λ\t%",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "arg=<do>\narg=<customCommand>\narg=<--module>\narg=<api>\narg=<--module>\narg=<a b>\narg=<>\narg=<λ\t%>\ncwd={}/checkout/app\nbuild={}/state/kotlin-toolchain/projects/build\ncache={}/state/kotlin-toolchain/shared\ndisplay=:7\n",
            dir.display(),
            dir.display(),
            dir.display()
        )
    );
    fs::remove_file(root.join("kotlin")).unwrap();
    let missing = kotlin_client(&dir, port, &["build"]).output().unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("kotlin-run:"));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no executable kotlin"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cancelling_kotlin_releases_the_shared_queue_for_cargo() {
    let dir = scratch("kotlin-queue");
    let root = dir.join("checkout");
    fs::write(root.join("module.yaml"), "product: jvm/app\n").unwrap();
    fs::copy(stand_in(&dir), root.join("kotlin")).unwrap();
    let port = 24300 + (std::process::id() % 1000) as u16;
    let _server = start_server(&dir, port);
    let mut kotlin = kotlin_client(&dir, port, &["slow", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while markers(&dir).is_empty() && Instant::now() < deadline {
        sleep(Duration::from_millis(50));
    }
    let pids = markers(&dir);
    let mut next = cargo_client(&dir, port, &["fast"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(300));
    let running = markers(&dir).len();
    kotlin.kill().unwrap();
    kotlin.wait().unwrap();
    let mut stderr = next.stderr.take().unwrap();
    let status = finishes(next, Duration::from_secs(10));
    let mut diagnostic = String::new();
    stderr.read_to_string(&mut diagnostic).unwrap();
    assert_eq!(pids.len(), 1, "Kotlin must run before Cargo is queued");
    assert_eq!(running, 1, "Cargo must share Kotlin's queue");
    assert_eq!(status.and_then(|s| s.code()), Some(3));
    assert!(diagnostic.contains("waiting: 1 build(s)"), "{diagnostic}");
    assert!(!alive(&pids[0]), "Kotlin survived cancellation");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cargo_preserves_arguments_cwd_environment_and_status_without_a_gradle_wrapper() {
    let dir = scratch("cargo");
    fs::remove_file(dir.join("checkout/gradlew")).unwrap();
    let script = dir.join("cargo.sh");
    fs::write(
        &script,
        "#!/bin/sh\nfor arg do printf '<%s>\\n' \"$arg\"; done\nprintf 'cwd=%s\\ncargo=%s\\ntarget=%s\\nrustup=%s\\nauto=%s\\n' \"$PWD\" \"$CARGO_HOME\" \"$CARGO_TARGET_DIR\" \"$RUSTUP_HOME\" \"$RUSTUP_AUTO_INSTALL\"\necho \"display: $DISPLAY\" >&2\nexit 7\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let port = 20300 + (std::process::id() % 1000) as u16;
    let mut server = server_command(&dir, port, "http://mirror:8080", 1);
    server.env("AGENT_RUN_BUILD_COMMAND", script);
    let _server = Server::start(server);
    let output = cargo_client(&dir, port, &["+1.99.1", "test", "--", "a b", "", "λ%\\t"])
        .output()
        .unwrap();
    let expected = format!(
        "<+1.99.1>\n<test>\n<-->\n<a b>\n<>\n<λ%\\t>\ncwd={}/checkout/app\ncargo={}/state/rust/cargo\ntarget={}/state/rust/targets/app\nrustup=/opt/rustup\nauto=0\n",
        dir.display(),
        dir.display(),
        dir.display()
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    assert!(String::from_utf8_lossy(&output.stderr).contains("display: :7"));
    assert_eq!(output.status.code(), Some(7));

    let output = client(&dir, port, &["test"]).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("gradle-run: no gradlew"));
    let mut outside = cargo_client(&dir, port, &["test"]);
    outside.current_dir(&dir);
    let output = outside.output().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("cargo-run:"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside the checkout"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cargo_and_gradle_share_the_queue_and_cargo_cancellation_frees_it() {
    let dir = scratch("mixed");
    let port = 21300 + (std::process::id() % 1000) as u16;
    let _server = start_server(&dir, port);
    let mut cargo = cargo_client(&dir, port, &["slow", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while markers(&dir).is_empty() && Instant::now() < deadline {
        sleep(Duration::from_millis(50));
    }
    let pids = markers(&dir);
    assert_eq!(
        pids.len(),
        1,
        "Cargo must be running before Gradle is queued"
    );
    let mut gradle = client(&dir, port, &["fast"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(300));
    assert_eq!(markers(&dir).len(), 1, "Gradle must share Cargo's queue");
    cargo.kill().unwrap();
    cargo.wait().unwrap();
    let mut stderr = gradle.stderr.take().unwrap();
    assert_eq!(
        finishes(gradle, Duration::from_secs(10)).and_then(|s| s.code()),
        Some(3)
    );
    let mut diagnostic = String::new();
    stderr.read_to_string(&mut diagnostic).unwrap();
    assert!(diagnostic.contains("waiting: 1 build(s)"));
    assert!(!alive(&pids[0]), "Cargo's process survived cancellation");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn runs_streams_queues_and_cancels() {
    let dir = scratch("local");
    let port = 17300 + (std::process::id() % 1000) as u16;
    let _server = start_server(&dir, port);

    // Output, DISPLAY and the exit status come back.
    let output = client(&dir, port, &[":app:test", "--tests", "a b"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "args: :app:test --tests a b\n"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("display: :7"),
        "{output:?}"
    );
    assert_eq!(output.status.code(), Some(3));

    // A second build waits for the first and says so.
    let started = Instant::now();
    let first = client(&dir, port, &["slow", "2"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(300));
    let second = client(&dir, port, &["fast"]).output().unwrap();
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "the second build didn't wait"
    );
    assert!(
        String::from_utf8_lossy(&second.stderr)
            .contains("waiting: 1 build(s) of this project running"),
        "{second:?}"
    );
    let _ = first.wait_with_output();

    // Killing the client (Ctrl+C) stops its build.
    let mut slow = client(&dir, port, &["slow", "30"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(500));
    let pids = markers(&dir);
    assert_eq!(pids.len(), 1, "the build runs");
    slow.kill().unwrap();
    let _ = slow.wait();
    let deadline = Instant::now() + Duration::from_secs(3);
    while alive(&pids[0]) && Instant::now() < deadline {
        sleep(Duration::from_millis(100));
    }
    assert!(
        !alive(&pids[0]),
        "the build still runs after its client went"
    );

    // Folders outside the checkout are refused.
    let mut outside = client(&dir, port, &["x"]);
    outside.current_dir(&dir);
    let refused = outside.output().unwrap();
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("outside the checkout"),
        "{refused:?}"
    );

    // Without a server, the client says what's missing.
    let mut none = client(&dir, port + 1, &[]).output().unwrap().stderr;
    let mut text = String::new();
    none.as_slice().read_to_string(&mut text).unwrap();
    none.clear();
    assert!(text.contains("doesn't answer on 127.0.0.1"), "{text}");
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_cancelled_build_takes_what_it_started_along_and_frees_its_slot() {
    let dir = scratch("pipes");
    let port = 18300 + (std::process::id() % 1000) as u16;
    let _server = start_server(&dir, port);
    // The build starts a process that keeps its output open, then waits.
    let mut build = client(&dir, port, &["pipes"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dir.join("running.child").exists() && Instant::now() < deadline {
        sleep(Duration::from_millis(100));
    }
    let descendant = fs::read_to_string(dir.join("running.child")).expect("the build's descendant");
    build.kill().unwrap();
    let _ = build.wait();
    // The next build gets the slot.
    let next = client(&dir, port, &["fast"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status = finishes(next, Duration::from_secs(10));
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(3),
        "the next build didn't run within 10 s"
    );
    assert!(
        !alive(&descendant),
        "the cancelled build's descendant still runs"
    );
    let _ = fs::remove_dir_all(dir);
}

/// A mirror on this machine that serves `files`, or holds every request while `stalled`.
fn mirror(
    files: Vec<(String, Vec<u8>)>,
    stalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let files = files.clone();
            let stalled = stalled.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                while stalled.load(std::sync::atomic::Ordering::SeqCst) {
                    sleep(Duration::from_millis(100));
                }
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let mut stream = stream;
                match files.iter().find(|(name, _)| path.ends_with(name.as_str())) {
                    Some((_, body)) => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(body);
                    }
                    None => {
                        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                }
            });
        }
    });
    url
}

#[test]
fn a_cancelled_install_launches_nothing_and_a_retry_installs() {
    let dir = scratch("install");
    let port = 19300 + (std::process::id() % 1000) as u16;
    // The checkout's wrapper: a distribution from the official URL, and a gradlew that records
    // each launch.
    let checkout = dir.join("checkout");
    fs::create_dir_all(checkout.join("gradle/wrapper")).unwrap();
    fs::write(
        checkout.join("gradle/wrapper/gradle-wrapper.properties"),
        "distributionUrl=https\\://services.gradle.org/distributions/gradle-0.1-bin.zip\n",
    )
    .unwrap();
    fs::write(
        checkout.join("gradlew"),
        format!(
            "#!/bin/sh\necho launched >> {}\n",
            dir.join("launched").display()
        ),
    )
    .unwrap();
    fs::set_permissions(checkout.join("gradlew"), fs::Permissions::from_mode(0o755)).unwrap();
    // The distribution, and its checksum.
    fs::create_dir_all(dir.join("dist/gradle-0.1/bin")).unwrap();
    fs::write(dir.join("dist/gradle-0.1/bin/gradle"), "").unwrap();
    assert!(
        Command::new("zip")
            .args(["-qr", "../dist.zip", "gradle-0.1"])
            .current_dir(dir.join("dist"))
            .status()
            .unwrap()
            .success()
    );
    let zip = fs::read(dir.join("dist.zip")).unwrap();
    let sum = String::from_utf8(
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(dir.join("dist.zip"))
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let files = vec![
        ("gradle-0.1-bin.zip.sha256".to_owned(), sum.into_bytes()),
        ("gradle-0.1-bin.zip".to_owned(), zip),
    ];
    let stalled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let url = mirror(files, stalled.clone());
    let _server = Server::start(server_command(&dir, port, &url, 2));

    // One build downloads (stalled), another waits for the installer; both are cancelled.
    let mut first = client(&dir, port, &["x"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(500));
    let mut second = client(&dir, port, &["x"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    sleep(Duration::from_millis(500));
    first.kill().unwrap();
    second.kill().unwrap();
    let _ = (first.wait(), second.wait());
    // The cancelled download stops.
    let downloads = || {
        String::from_utf8(
            Command::new("pgrep")
                .args(["-f", &format!("curl.*{url}")])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while !downloads().is_empty() && Instant::now() < deadline {
        sleep(Duration::from_millis(100));
    }
    assert_eq!(downloads(), "", "a cancelled download still runs");
    stalled.store(false, std::sync::atomic::Ordering::SeqCst);

    // A retry installs and launches once; the cancelled ones never launch.
    let retry = client(&dir, port, &["x"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status = finishes(retry, Duration::from_secs(15));
    sleep(Duration::from_millis(500));
    let launched = fs::read_to_string(dir.join("launched")).unwrap_or_default();
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(0),
        "the retry; launched: {launched:?}"
    );
    assert_eq!(launched.lines().count(), 1, "launches: {launched:?}");
    let _ = fs::remove_dir_all(dir);
}
