//! A session's display and browser: Xvfb and a Playwright
//! MCP server in the project's build container, one of each per session, numbered within the
//! pod, started and stopped with the session (`agent-run-display` in the build image).

use pod::Project;
use podman::{Object, Podman};

/// The label with a session's display number, on its container.
pub const DISPLAY_LABEL: &str = "agent-run.display";
/// Where the browser writes screenshots, in the build state agents read.
const SCREENSHOTS: &str = "/build-state/screenshots";

/// The smallest display number no session in the pod uses, running or stopped (its launcher
/// still collects its status and cleans up). Called under the start lock, so two sessions never
/// pick the same one.
pub fn free_display(podman: &Podman, project: &Project) -> Result<u32, String> {
    let err = |e: podman::PodmanError| e.to_string();
    if !podman.exists(Object::Pod, &project.pod()).map_err(err)? {
        return Ok(1);
    }
    let names = podman
        .run(&[
            "ps",
            "--all",
            "--filter",
            &format!("pod={}", project.pod()),
            "--format",
            "{{.Names}}",
        ])
        .map_err(err)?;
    let mut used = Vec::new();
    for name in names.lines().filter(|n| !n.is_empty()) {
        // `ps` can't index labels in its template; `inspect` can.
        if let Some(n) = podman
            .labels(Object::Container, name)
            .map_err(err)?
            .get(DISPLAY_LABEL)
            .and_then(|n| n.parse::<u32>().ok())
        {
            used.push(n);
        }
    }
    Ok((1..).find(|n| !used.contains(n)).unwrap_or(1))
}

/// The port of the session's browser server on the pod's localhost.
pub fn browser_port(display: u32) -> u32 {
    7400 + display
}

/// What the session's container gets: its display as a label (for [`free_display`]) and as
/// `DISPLAY`.
pub fn container_args(display: u32) -> Vec<String> {
    vec![
        "--label".into(),
        format!("{DISPLAY_LABEL}={display}"),
        "-e".into(),
        format!("DISPLAY=:{display}"),
    ]
}

/// The browser's MCP endpoint (streamable HTTP), for the agent's configuration.
pub fn browser_url(display: u32) -> String {
    format!("http://127.0.0.1:{}/mcp", browser_port(display))
}

/// Starts the session's display and browser in the build container.
pub fn start(
    podman: &Podman,
    project: &Project,
    display: u32,
    session: &str,
) -> Result<(), String> {
    let screenshots = format!("{SCREENSHOTS}/{session}");
    podman
        .run(&[
            "exec",
            "-d",
            &project.build_container(),
            "agent-run-display",
            "start",
            &display.to_string(),
            &screenshots,
        ])
        .map(|_| ())
        .map_err(|e| format!("starting the session's display: {e}"))
}

/// Stops the session's display and browser.
pub fn stop(podman: &Podman, project: &Project, display: u32) -> Result<(), String> {
    podman
        .run(&[
            "exec",
            &project.build_container(),
            "agent-run-display",
            "stop",
            &display.to_string(),
        ])
        .map(|_| ())
        .map_err(|e| format!("stopping the session's display: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_display_has_its_own_browser() {
        assert_eq!(
            container_args(2),
            ["--label", "agent-run.display=2", "-e", "DISPLAY=:2"]
        );
        assert_eq!(browser_url(2), "http://127.0.0.1:7402/mcp");
    }
}
