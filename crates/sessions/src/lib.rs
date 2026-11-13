//! An agent session's container: the agent's command behind the
//! gate, with the checkout's mount plan, the agent's folders from the host, the proxy and the
//! relay in its environment, and the build state to read.

use mountplan::Plan;
use pod::Project;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The agent's folders on the host.
pub struct Host {
    pub agent_home: PathBuf,
    /// Global instructions, given read-only.
    pub instructions: PathBuf,
    /// Shared reference material, read-only at `/knowledge`.
    pub knowledge: PathBuf,
}

/// The session container's settings.
pub struct Settings<'a> {
    pub name: &'a str,
    pub image: &'a str,
    pub memory: u64,
    pub cpus: f64,
    pub relay_url: &'a str,
    /// The proxy with the session's own credentials: it decides what the session reaches.
    pub proxy_url: &'a str,
    /// Arguments other problems add (the session's display, engram's mounts).
    pub extra: Vec<String>,
    /// Where the agent's CLI loads its global instructions, relative to its home.
    pub instructions_at: &'a str,
    /// Lines added to the global instructions (engram's entry point for some agents).
    pub instructions_extra: Option<&'a str>,
    /// The agent's environment and read-only files (content, path), from its CLI's conventions.
    pub env: &'a [(String, String)],
    pub files: &'a [(String, String)],
    /// Holds this session's copies of host files; removed with the session.
    pub session_dir: &'a Path,
}

/// `podman run` arguments for the session: started detached with a terminal, the command
/// behind the gate.
pub fn run_args(
    project: &Project,
    plan: &Plan,
    host: &Host,
    settings: &Settings,
    command: &[String],
) -> Result<Vec<String>, String> {
    // No --rm: the exit status is read after the command ends; the session's end removes it.
    let mut args: Vec<String> = [
        "run",
        "-d",
        "-t",
        "-i",
        "--pod",
        &project.pod(),
        "--name",
        settings.name,
    ]
    .map(String::from)
    .into();
    args.extend(["--label".into(), project.namespace.label()]);
    args.extend(
        [
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            "1024",
        ]
        .map(String::from),
    );
    args.extend([
        "--memory".into(),
        settings.memory.to_string(),
        "--cpus".into(),
        settings.cpus.to_string(),
    ]);
    args.extend(["--workdir".into(), project.checkout.display().to_string()]);
    for (key, value) in [
        ("HTTPS_PROXY", settings.proxy_url),
        ("HTTP_PROXY", settings.proxy_url),
        ("https_proxy", settings.proxy_url),
        ("http_proxy", settings.proxy_url),
        // The relay is reached directly on the pod's network, not through the proxy.
        ("NO_PROXY", "localhost,127.0.0.1,github-relay"),
        ("no_proxy", "localhost,127.0.0.1,github-relay"),
        ("GITHUB_API_URL", settings.relay_url),
        ("NODE_USE_ENV_PROXY", "1"),
        ("DISABLE_AUTOUPDATER", "1"),
    ] {
        args.extend(["-e".into(), format!("{key}={value}")]);
    }
    // CLIs detect the terminal from these; Podman would set TERM=xterm.
    for key in [
        "TERM",
        "COLORTERM",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "TERMINAL_EMULATOR",
    ] {
        if let Ok(value) = std::env::var(key) {
            args.extend(["-e".into(), format!("{key}={value}")]);
        }
    }
    for (key, value) in settings.env {
        args.extend(["-e".into(), format!("{key}={value}")]);
    }
    args.extend(host_mounts(host, settings, global_ignore().as_deref())?);
    for (i, (content, target)) in settings.files.iter().enumerate() {
        let copy = settings.session_dir.join(format!("file-{i}"));
        fs::create_dir_all(settings.session_dir)
            .map_err(|e| format!("{}: {e}", settings.session_dir.display()))?;
        fs::write(&copy, content).map_err(|e| format!("{}: {e}", copy.display()))?;
        args.extend(["-v".into(), format!("{}:{target}:ro,z", copy.display())]);
    }
    args.extend(builds::agent_mount(project));
    args.extend(settings.extra.iter().cloned());
    args.extend(plan.podman_args());
    // The gate announces that it waits, so the launcher knows when the terminal is attached.
    args.extend([settings.image.to_owned(), "attach".into(), "--".into()]);
    args.extend(command.iter().cloned());
    Ok(args)
}

/// Where the container's Git finds the host's global ignore file.
const IGNORE_IN_CONTAINER: &str = "/etc/agent-run/gitignore";

/// The host's global ignore file, found as Git finds it: `core.excludesFile`, else
/// `$XDG_CONFIG_HOME/git/ignore` or `~/.config/git/ignore`.
fn global_ignore() -> Option<PathBuf> {
    let configured = Command::new("git")
        .args(["config", "--global", "--get", "--path", "core.excludesFile"])
        .output()
        .ok()?;
    let configured = String::from_utf8_lossy(&configured.stdout)
        .trim()
        .to_owned();
    let path = if configured.is_empty() {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        config.join("git/ignore")
    } else {
        PathBuf::from(configured)
    };
    path.is_file().then_some(path)
}

/// The agent's own folders from the host: its home, the global instructions and the global
/// ignore file (copies, read-only, so neither side can change the other's), and the shared
/// knowledge, read-only as a folder, so files replaced on the host stay visible.
fn host_mounts(
    host: &Host,
    settings: &Settings,
    global_ignore: Option<&Path>,
) -> Result<Vec<String>, String> {
    let io = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    let session_dir = settings.session_dir;
    fs::create_dir_all(&host.agent_home).map_err(|e| io(&host.agent_home, e))?;
    let mut args = vec![
        "-v".to_owned(),
        format!("{}:/home/node:z", host.agent_home.display()),
    ];
    let mut instructions = if host.instructions.is_file() {
        fs::read_to_string(&host.instructions).map_err(|e| io(&host.instructions, e))?
    } else {
        String::new()
    };
    if let Some(extra) = settings.instructions_extra {
        instructions = if instructions.is_empty() {
            format!("{extra}\n")
        } else {
            format!("{}\n\n{extra}\n", instructions.trim_end())
        };
    }
    if !instructions.is_empty() {
        fs::create_dir_all(session_dir).map_err(|e| io(session_dir, e))?;
        let copy = session_dir.join("instructions.md");
        fs::write(&copy, instructions).map_err(|e| io(&copy, e))?;
        args.extend([
            "-v".into(),
            format!(
                "{}:/home/node/{}:ro,z",
                copy.display(),
                settings.instructions_at
            ),
        ]);
    }
    if host.knowledge.is_dir() {
        args.extend([
            "-v".into(),
            format!("{}:/knowledge:ro,z", host.knowledge.display()),
        ]);
    }
    // The container's Git ignores what the host's ignores (engram's links, editor files).
    if let Some(ignore) = global_ignore {
        fs::create_dir_all(session_dir).map_err(|e| io(session_dir, e))?;
        let copy = session_dir.join("gitignore");
        fs::copy(ignore, &copy).map_err(|e| io(&copy, e))?;
        args.extend([
            "-v".into(),
            format!("{}:{IGNORE_IN_CONTAINER}:ro,z", copy.display()),
        ]);
        for env in [
            "GIT_CONFIG_COUNT=1",
            "GIT_CONFIG_KEY_0=core.excludesFile",
            &format!("GIT_CONFIG_VALUE_0={IGNORE_IN_CONTAINER}"),
        ] {
            args.extend(["-e".into(), env.to_owned()]);
        }
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_paths() -> (Host, PathBuf) {
        let base = std::env::temp_dir().join(format!("agent-session-test-{}", std::process::id()));
        let host = Host {
            agent_home: base.join("claude/home"),
            instructions: base.join("instructions.md"),
            knowledge: base.join("knowledge"),
        };
        (host, base.join("state/sessions/test"))
    }

    #[test]
    fn mounts_home_instructions_and_knowledge_from_the_host() {
        let (paths, session_dir) = scratch_paths();
        let settings = |extra: Option<&'static str>| Settings {
            name: "s",
            image: "i",
            memory: 1,
            cpus: 2.0,
            relay_url: "r",
            proxy_url: "http://u:p@proxy:3128",
            extra: vec![],
            instructions_at: ".codex/AGENTS.md",
            instructions_extra: extra,
            env: &[],
            files: &[],
            session_dir: Box::leak(session_dir.clone().into_boxed_path()),
        };
        let args = host_mounts(&paths, &settings(None), None)
            .unwrap()
            .join(" ");
        assert!(args.contains(&format!("{}:/home/node:z", paths.agent_home.display())));
        assert!(
            !args.contains("/knowledge")
                && !args.contains("AGENTS.md")
                && !args.contains("GIT_CONFIG"),
            "only what exists: {args}"
        );

        fs::create_dir_all(&paths.knowledge).unwrap();
        fs::write(&paths.instructions, "be careful").unwrap();
        let ignore = paths.agent_home.parent().unwrap().join("host-gitignore");
        fs::create_dir_all(paths.agent_home.parent().unwrap()).unwrap();
        fs::write(&ignore, "/AGENTS.local.md\n").unwrap();
        let args = host_mounts(
            &paths,
            &settings(Some("Read AGENTS.local.md too.")),
            Some(&ignore),
        )
        .unwrap()
        .join(" ");
        assert!(args.contains(&format!("{}:/knowledge:ro,z", paths.knowledge.display())));
        assert!(args.contains(":/etc/agent-run/gitignore:ro,z"));
        assert!(args.contains(
            "-e GIT_CONFIG_KEY_0=core.excludesFile -e GIT_CONFIG_VALUE_0=/etc/agent-run/gitignore"
        ));
        assert_eq!(
            fs::read_to_string(session_dir.join("gitignore")).unwrap(),
            "/AGENTS.local.md\n"
        );
        assert!(args.contains(":/home/node/.codex/AGENTS.md:ro,z"), "{args}");
        assert_eq!(
            fs::read_to_string(session_dir.join("instructions.md")).unwrap(),
            "be careful\n\nRead AGENTS.local.md too.\n"
        );
    }
}
