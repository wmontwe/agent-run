//! Each session's display and browser in the build container:
//! two sessions in one pod get their own, a third pod sees neither, and ending a
//! session ends only its own. The display and browser on their own:
//! `crates/vision/tests/podman.rs`. Needs the Podman machine.
//! Run with `cargo test -p agent --test vision -- --ignored`.

mod common;

use agent::session::{self, Session};
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::Duration;

fn exec(container: &str, env: &[(&str, &str)], script: &str) -> Output {
    let mut command = Command::new("podman");
    command.arg("exec");
    for (key, value) in env {
        command.args(["-e", &format!("{key}={value}")]);
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

/// Calls the session's browser server as the agent's CLI does (MCP over HTTP), from the
/// agent's container: initialize, then each tool call. Prints every response.
fn browser(session: &Session, calls: &[(&str, &str)]) -> String {
    let url = format!(
        "http://127.0.0.1:{}/mcp",
        vision::browser_port(session.display)
    );
    let mut script = format!(
        r#"set -e
u={url}
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
    text(&exec(&session.name, &[], &script))
}

const WINDOW: &str = r#"import javax.swing.*;
public class Window {
    public static void main(String[] args) throws Exception {
        SwingUtilities.invokeLater(() -> {
            JFrame frame = new JFrame("agent-run-window");
            frame.setSize(400, 300);
            frame.setVisible(true);
        });
        Thread.sleep(120_000);
    }
}
"#;

#[test]
#[ignore = "needs the Podman machine"]
fn each_session_sees_its_own_display_and_browser_only() {
    let root = common::checkout("vision");
    let other_root = common::checkout("vision-other");
    let scratch = common::Scratch::new("vision");
    scratch.policy("version = 1\n");
    let sleep_cmd = ["sleep".to_owned(), "600".to_owned()];
    let first =
        session::start("claude", &root, Some(&sleep_cmd), scratch.paths()).expect("first session");
    let second =
        session::start("claude", &root, Some(&sleep_cmd), scratch.paths()).expect("second session");
    let other = session::start("claude", &other_root, Some(&sleep_cmd), scratch.paths())
        .expect("session in another pod");
    let build = first.project.build_container();
    let display = format!(":{}", first.display);
    let mut failures = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        if !ok {
            failures.push(format!("{name}: {detail}"));
        }
    };
    sleep(Duration::from_secs(3));

    check(
        "two sessions in one pod get two displays",
        first.display != second.display,
        format!("{} and {}", first.display, second.display),
    );

    // A page served by the build container, for the isolation checks below.
    exec(
        &build,
        &[],
        "mkdir -p /tmp/site && echo '<title>agent-run-page</title><h1>hello</h1>' > /tmp/site/index.html",
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
    sleep(Duration::from_secs(2));

    // A window from the build container, on the first session's display only.
    exec(
        &build,
        &[],
        &format!("cat > /tmp/Window.java <<'EOF'\n{WINDOW}EOF"),
    );
    Command::new("podman")
        .args([
            "exec",
            "-d",
            "-e",
            &format!("DISPLAY={display}"),
            &build,
            "/opt/jdk/21/bin/java",
            "/tmp/Window.java",
        ])
        .output()
        .unwrap();
    sleep(Duration::from_secs(8));
    let windows = |session: &Session| text(&exec(&session.name, &[], "xwininfo -root -tree"));
    let seen = windows(&first);
    check(
        "the window shows on its session's display",
        seen.contains("agent-run-window"),
        seen.chars().take(400).collect(),
    );
    check(
        "the window isn't on the other session's display",
        !windows(&second).contains("agent-run-window"),
        String::new(),
    );

    // Isolation: another pod sees neither the window nor this pod's browser.
    check(
        "another pod doesn't see the window",
        !windows(&other).contains("agent-run-window"),
        String::new(),
    );
    let other_page = browser(
        &other,
        &[("browser_navigate", r#"{"url":"http://localhost:8800/"}"#)],
    );
    check(
        "another pod's browser can't open this pod's server",
        !other_page.contains("agent-run-page"),
        other_page.chars().take(300).collect(),
    );

    // Ending a session stops its browser and leaves the other's.
    let second_port = vision::browser_port(second.display);
    second.end();
    let gone = exec(
        &first.name,
        &[],
        &format!("curl -s --max-time 3 http://127.0.0.1:{second_port}/mcp"),
    );
    check(
        "an ended session's browser is gone",
        !gone.status.success(),
        text(&gone),
    );
    let still = browser(
        &first,
        &[("browser_navigate", r#"{"url":"http://localhost:8800/"}"#)],
    );
    check(
        "the remaining session's browser still works",
        still.contains("agent-run-page"),
        still.chars().take(300).collect(),
    );

    first.end();
    other.end();
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&other_root);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
