//! A session's display and browser on their own: started
//! in a build container, used from a container sharing its network namespace as a pod's
//! containers do, stopped again. Needs the Podman machine, `localhost/agent-run-build` and an
//! agent image (`AGENT_RUN_TEST_IMAGE`, default `localhost/agent-run-claude`).
//! Run with `cargo test -p vision -- --ignored`.

use pod::Project;
use podman::{Namespace, Podman};
use std::path::Path;
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::Duration;

fn exec(container: &str, env: &[&str], script: &str) -> Output {
    let mut command = Command::new("podman");
    command.arg("exec");
    for value in env {
        command.args(["-e", value]);
    }
    command
        .args([container, "sh", "-c", script])
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// MCP over HTTP to the browser server, as the agent's CLI talks to it.
fn browser(client: &str, port: u32, calls: &[(&str, &str)]) -> String {
    let mut script = format!(
        r#"u=http://127.0.0.1:{port}/mcp
h='-H Content-Type:application/json -H Accept:application/json,text/event-stream'
id=$(curl -s -D - -o /dev/null $h -d '{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-03-26","capabilities":{{}},"clientInfo":{{"name":"test","version":"1"}}}}}}' $u | tr -d '\r' | sed -n 's/^mcp-session-id: //Ip')
curl -s $h -H "mcp-session-id: $id" -d '{{"jsonrpc":"2.0","method":"notifications/initialized"}}' $u >/dev/null
"#
    );
    for (i, (tool, arguments)) in calls.iter().enumerate() {
        script += &format!(
            "curl -s --max-time 60 $h -H \"mcp-session-id: $id\" -d '{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"tools/call\",\"params\":{{\"name\":\"{tool}\",\"arguments\":{arguments}}}}}' $u\necho\n",
            i + 2
        );
    }
    text(&exec(client, &[], &script))
}

const WINDOW: &str = "import javax.swing.*; public class Window { public static void main(String[] a) throws Exception { \
    SwingUtilities.invokeLater(() -> { JFrame f = new JFrame(\"agent-run-window\"); f.setSize(400, 300); f.setVisible(true); }); Thread.sleep(120000); } }";

/// Removes the test's namespace when the test ends, also when it fails.
struct Cleanup(Namespace);

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.remove_all(&Podman::default(), &[]);
    }
}

#[test]
#[ignore = "needs the Podman machine and the images"]
fn a_display_and_browser_start_serve_and_stop() {
    let podman = Podman::default();
    let ns = Namespace::new(&format!("vision-test-{}", std::process::id()));
    let _cleanup = Cleanup(ns.clone());
    let project = Project::new(&ns, Path::new("/x/app"));
    let build = project.build_container();
    let client = ns.name("client");
    let agent_image = std::env::var("AGENT_RUN_TEST_IMAGE")
        .unwrap_or_else(|_| "localhost/agent-run-claude".to_owned());
    // /build-state is the build state volume in a pod; a tmpfs here.
    podman
        .run(&[
            "run",
            "-d",
            "--name",
            &build,
            "--label",
            &ns.label(),
            "--read-only",
            "--tmpfs",
            "/build-state",
            "--entrypoint",
            "sleep",
            "localhost/agent-run-build",
            "300",
        ])
        .unwrap();
    // Sharing the build container's network namespace, as a pod's containers do.
    podman
        .run(&[
            "run",
            "-d",
            "--name",
            &client,
            "--label",
            &ns.label(),
            "--network",
            &format!("container:{build}"),
            "--entrypoint",
            "sleep",
            &agent_image,
            "300",
        ])
        .unwrap();
    let (display, port) = (2, vision::browser_port(2));
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        if !ok {
            failures.push(format!("{name}: {detail}"));
        }
    };

    vision::start(&podman, &project, display, "session").unwrap();
    exec(
        &build,
        &[],
        "mkdir -p /tmp/site && echo '<title>agent-run-page</title>' > /tmp/site/index.html",
    );
    Command::new("podman")
        .args([
            "exec",
            "-d",
            &build,
            "/opt/jdk/21/bin/jwebserver",
            "-b",
            "127.0.0.1",
            "-p",
            "8800",
            "-d",
            "/tmp/site",
        ])
        .output()
        .unwrap();
    exec(&build, &[], &format!("echo '{WINDOW}' > /tmp/Window.java"));
    Command::new("podman")
        .args([
            "exec",
            "-d",
            "-e",
            "DISPLAY=:2",
            &build,
            "/opt/jdk/21/bin/java",
            "/tmp/Window.java",
        ])
        .output()
        .unwrap();
    sleep(Duration::from_secs(8));

    let page = browser(
        &client,
        port,
        &[
            ("browser_navigate", r#"{"url":"http://localhost:8800/"}"#),
            ("browser_take_screenshot", "{}"),
        ],
    );
    check(
        "the browser opens a page on localhost",
        page.contains("agent-run-page"),
        page.chars().take(400).collect(),
    );
    check(
        "the screenshot comes back as an image",
        page.contains("image/png") || page.contains("image/jpeg"),
        String::new(),
    );
    let shots = exec(&build, &[], "ls /build-state/screenshots/session");
    check(
        "screenshots land in the build state",
        shots.status.success() && !text(&shots).trim().is_empty(),
        text(&shots),
    );
    let windows = text(&exec(&client, &["DISPLAY=:2"], "xwininfo -root -tree"));
    check(
        "the window shows on the display",
        windows.contains("agent-run-window"),
        windows.chars().take(300).collect(),
    );
    let shot = exec(
        &client,
        &["DISPLAY=:2"],
        "scrot /tmp/screen.png && test -s /tmp/screen.png",
    );
    check(
        "scrot captures the display",
        shot.status.success(),
        text(&shot),
    );
    let tcp = exec(&client, &[], "bash -c 'echo > /dev/tcp/127.0.0.1/6002'");
    check(
        "the display doesn't listen on TCP",
        !tcp.status.success(),
        String::new(),
    );

    vision::stop(&podman, &project, display).unwrap();
    sleep(Duration::from_secs(1));
    let gone = exec(
        &client,
        &[],
        &format!("curl -s --max-time 3 http://127.0.0.1:{port}/mcp"),
    );
    check("stop ends the browser", !gone.status.success(), text(&gone));
    let display_gone = exec(&client, &["DISPLAY=:2"], "xwininfo -root");
    check(
        "stop ends the display",
        !display_gone.status.success(),
        String::new(),
    );

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
