//! The agent CLIs agent-run runs: how each
//! takes the session's MCP servers without anything written to its home, where it loads global
//! instructions, and what else it needs in the container.

use serde_json::{Map, Value, json};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
    Pi,
}

/// An MCP server for the session: a URL (streamable HTTP) or a command (stdio).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Server {
    Http {
        name: String,
        url: String,
    },
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
    },
}

impl Server {
    fn name(&self) -> &str {
        match self {
            Server::Http { name, .. } | Server::Stdio { name, .. } => name,
        }
    }
}

/// What the session's container needs to start the agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    pub command: Vec<String>,
    pub env: Vec<(String, String)>,
    /// A file mounted read-only: its content and where.
    pub files: Vec<(String, String)>,
    /// A file that must belong to root (Gemini's system defaults): a Podman secret.
    pub root_file: Option<(String, String)>,
}

/// Where the agent's home is in its container.
pub const HOME: &str = "/home/node";
const GEMINI_DEFAULTS: &str = "/etc/gemini-cli/system-defaults.json";

impl Agent {
    pub const ALL: [Agent; 4] = [Agent::Claude, Agent::Codex, Agent::Gemini, Agent::Pi];

    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Gemini => "gemini",
            Agent::Pi => "pi",
        }
    }

    pub fn parse(name: &str) -> Option<Agent> {
        Agent::ALL.into_iter().find(|a| a.name() == name)
    }

    /// Where the CLI loads its global instructions, relative to its home.
    pub fn instructions(self) -> &'static str {
        match self {
            Agent::Claude => ".claude/CLAUDE.md",
            Agent::Codex => ".codex/AGENTS.md",
            Agent::Gemini => ".gemini/GEMINI.md",
            Agent::Pi => ".pi/agent/AGENTS.md",
        }
    }

    /// Whether it reads a project's `CLAUDE.local.md` by itself, and so engram's entry point;
    /// the others are told in their global instructions.
    pub fn reads_claude_local(self) -> bool {
        self == Agent::Claude
    }

    /// How to start the agent with the session's MCP servers. `home` is the agent's home on the
    /// host, for Pi's own `mcp.json`.
    pub fn launch(self, servers: &[Server], home: &Path) -> Result<Launch, String> {
        Ok(match self {
            Agent::Claude => {
                let mut command = vec!["claude".to_owned()];
                if !servers.is_empty() {
                    let config = json!({ "mcpServers": servers.iter().map(|s| (s.name().to_owned(), claude(s))).collect::<Map<_, _>>() });
                    command.extend(["--mcp-config".to_owned(), config.to_string()]);
                }
                Launch {
                    command,
                    ..Launch::default()
                }
            }
            Agent::Codex => {
                // The container is the sandbox; Codex's own needs kernel features it doesn't allow.
                let mut command = vec![
                    "codex".to_owned(),
                    "-c".into(),
                    r#"sandbox_mode="danger-full-access""#.into(),
                ];
                for server in servers {
                    for (key, value) in codex(server) {
                        command.extend([
                            "-c".to_owned(),
                            format!("mcp_servers.{}.{key}={value}", server.name()),
                        ]);
                    }
                }
                Launch {
                    command,
                    ..Launch::default()
                }
            }
            Agent::Gemini => {
                // Folder trust disables every MCP server in untrusted folders; the container
                // replaces that protection.
                let mcp: Map<String, Value> = servers
                    .iter()
                    .map(|s| (s.name().to_owned(), gemini(s)))
                    .collect();
                let defaults = json!({ "security": { "folderTrust": { "enabled": false } }, "mcpServers": mcp });
                Launch {
                    command: vec!["gemini".into()],
                    env: vec![(
                        "GEMINI_CLI_SYSTEM_DEFAULTS_PATH".into(),
                        GEMINI_DEFAULTS.into(),
                    )],
                    root_file: Some((defaults.to_string(), GEMINI_DEFAULTS.into())),
                    ..Launch::default()
                }
            }
            Agent::Pi => {
                let own = home.join(".pi/agent/mcp.json");
                let target = format!("{HOME}/.pi/agent/mcp.json");
                // No network calls at Pi's start (its update check).
                let env = vec![("PI_OFFLINE".into(), "1".into())];
                Launch {
                    command: vec!["pi".into()],
                    env,
                    files: vec![(pi(&own, servers)?, target)],
                    ..Launch::default()
                }
            }
        })
    }
}

fn claude(server: &Server) -> Value {
    match server {
        Server::Http { url, .. } => json!({ "type": "http", "url": url }),
        Server::Stdio { command, args, .. } => json!({ "command": command, "args": args }),
    }
}

/// `-c` values for Codex, as TOML.
fn codex(server: &Server) -> Vec<(&'static str, String)> {
    let toml_string = |s: &str| Value::String(s.to_owned()).to_string();
    match server {
        Server::Http { url, .. } => vec![("url", toml_string(url))],
        Server::Stdio { command, args, .. } => {
            let args: Vec<String> = args.iter().map(|a| toml_string(a)).collect();
            vec![
                ("command", toml_string(command)),
                ("args", format!("[{}]", args.join(","))),
            ]
        }
    }
}

fn gemini(server: &Server) -> Value {
    match server {
        Server::Http { url, .. } => json!({ "httpUrl": url }),
        Server::Stdio { command, args, .. } => json!({ "command": command, "args": args }),
    }
}

/// Pi's own `mcp.json` (if any) with the session's servers added; the session's win.
fn pi(own: &Path, servers: &[Server]) -> Result<String, String> {
    let mut config = match fs::read_to_string(own) {
        Ok(text) => {
            serde_json::from_str::<Value>(&text).map_err(|e| format!("{}: {e}", own.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{}: {e}", own.display())),
    };
    let object = config
        .as_object_mut()
        .ok_or_else(|| format!("{}: expected an object", own.display()))?;
    let mcp = object.entry("mcpServers").or_insert_with(|| json!({}));
    let mcp = mcp
        .as_object_mut()
        .ok_or_else(|| format!("{}: mcpServers must be an object", own.display()))?;
    for server in servers {
        let entry = match server {
            Server::Http { url, .. } => json!({ "url": url }),
            Server::Stdio { command, args, .. } => json!({ "command": command, "args": args }),
        };
        mcp.insert(server.name().to_owned(), entry);
    }
    serde_json::to_string_pretty(&config).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn servers() -> Vec<Server> {
        vec![
            Server::Http {
                name: "playwright".into(),
                url: "http://127.0.0.1:7401/mcp".into(),
            },
            Server::Stdio {
                name: "engram".into(),
                command: "socat".into(),
                args: vec!["-".into(), "UNIX-CONNECT:/run/engram/mcp.sock".into()],
            },
        ]
    }

    #[test]
    fn claude_gets_json_on_its_command_line() {
        let launch = Agent::Claude
            .launch(&servers(), Path::new("/nowhere"))
            .unwrap();
        assert_eq!(launch.command[..2], ["claude", "--mcp-config"]);
        let config: Value = serde_json::from_str(&launch.command[2]).unwrap();
        assert_eq!(
            config["mcpServers"]["playwright"],
            json!({ "type": "http", "url": "http://127.0.0.1:7401/mcp" })
        );
        assert_eq!(
            config["mcpServers"]["engram"]["args"][1],
            "UNIX-CONNECT:/run/engram/mcp.sock"
        );
        assert_eq!(
            Agent::Claude
                .launch(&[], Path::new("/nowhere"))
                .unwrap()
                .command,
            ["claude"]
        );
    }

    #[test]
    fn codex_gets_toml_values_and_no_sandbox_of_its_own() {
        let command = Agent::Codex
            .launch(&servers(), Path::new("/nowhere"))
            .unwrap()
            .command
            .join(" ");
        assert!(
            command.starts_with(r#"codex -c sandbox_mode="danger-full-access""#),
            "{command}"
        );
        assert!(
            command.contains(r#"-c mcp_servers.playwright.url="http://127.0.0.1:7401/mcp""#),
            "{command}"
        );
        assert!(command.contains(r#"-c mcp_servers.engram.command="socat" -c mcp_servers.engram.args=["-","UNIX-CONNECT:/run/engram/mcp.sock"]"#), "{command}");
    }

    #[test]
    fn gemini_gets_root_owned_system_defaults() {
        let launch = Agent::Gemini
            .launch(&servers(), Path::new("/nowhere"))
            .unwrap();
        assert_eq!(
            launch.env,
            [(
                "GEMINI_CLI_SYSTEM_DEFAULTS_PATH".to_owned(),
                GEMINI_DEFAULTS.to_owned()
            )]
        );
        let (content, target) = launch.root_file.unwrap();
        let defaults: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(target, GEMINI_DEFAULTS);
        assert_eq!(defaults["security"]["folderTrust"]["enabled"], false);
        assert_eq!(
            defaults["mcpServers"]["playwright"]["httpUrl"],
            "http://127.0.0.1:7401/mcp"
        );
    }

    #[test]
    fn pi_gets_its_own_servers_and_the_sessions() {
        let home = std::env::temp_dir().join(format!("agents-pi-{}", std::process::id()));
        let own = home.join(".pi/agent/mcp.json");
        fs::create_dir_all(own.parent().unwrap()).unwrap();
        let fresh = Agent::Pi.launch(&servers(), &home).unwrap();
        let config: Value = serde_json::from_str(&fresh.files[0].0).unwrap();
        assert_eq!(
            config["mcpServers"]["playwright"]["url"],
            "http://127.0.0.1:7401/mcp"
        );
        assert_eq!(fresh.files[0].1, "/home/node/.pi/agent/mcp.json");
        fs::write(&own, r#"{"autoEnableCodemode":true,"mcpServers":{"mine":{"command":"x"},"playwright":{"url":"http://old"}}}"#).unwrap();
        let merged: Value =
            serde_json::from_str(&Agent::Pi.launch(&servers(), &home).unwrap().files[0].0).unwrap();
        assert_eq!(merged["mcpServers"]["mine"]["command"], "x");
        assert_eq!(
            merged["mcpServers"]["playwright"]["url"], "http://127.0.0.1:7401/mcp",
            "the session's win"
        );
        assert_eq!(merged["autoEnableCodemode"], true);
        fs::write(&own, "not json").unwrap();
        assert!(
            Agent::Pi
                .launch(&servers(), &home)
                .unwrap_err()
                .contains("mcp.json")
        );
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn every_agent_has_its_instructions() {
        assert_eq!(
            Agent::ALL.map(Agent::instructions),
            [
                ".claude/CLAUDE.md",
                ".codex/AGENTS.md",
                ".gemini/GEMINI.md",
                ".pi/agent/AGENTS.md"
            ]
        );
        assert_eq!(Agent::parse("pi"), Some(Agent::Pi));
        assert_eq!(Agent::parse("junie"), None);
    }
}
