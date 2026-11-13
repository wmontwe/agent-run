//! agent-run's command: parses the command line and runs a
//! session through the library. It holds no problem-specific logic of its own.
//!
//! ```text
//! agent claude|codex|gemini|pi [COMMAND [ARGS...]]   a session of that agent here: its CLI,
//!                                                     or the command instead (`agent claude bash`)
//! agent restart              recreate this checkout's pod, for example after a policy change
//! agent shutdown [--force]   stop everything agent-run runs; volumes stay. --force ends sessions
//! agent allow HOST[:PORT] [--session | --project] [--agent NAME]   approve for one agent
//! agent revoke HOST[:PORT] [--agent NAME | --global]                withdraw an approval
//! agent denied               what this project's sessions were refused
//! agent policy show          what the policy grants here
//! ```
//!
//! `allow`, `revoke`, `denied` and `policy show` are about the project whose checkout holds
//! the current folder.
//!
//! Configuration and state live in `~/.agent-run`, or in `$AGENT_RUN_HOME` (tests).

use agent::Paths;
use agent::approve;
use agent::session;
use approvals::Scope;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    // From here on, a SIGTERM, SIGHUP or SIGINT ends the session instead of the launcher alone.
    agent::signals::install();
    let names: Vec<&str> = agents::Agent::ALL.iter().map(|a| a.name()).collect();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let here = checkout;
    let paths = || Paths::from_env("claude");
    let done = |r: Result<(), String>| r.map(|()| ExitCode::SUCCESS);
    let result = match words[..] {
        ["restart"] => done(here().and_then(|c| session::restart(&c))),
        ["shutdown"] => done(paths().and_then(|p| session::shutdown(&p, false))),
        ["shutdown", "--force"] => done(paths().and_then(|p| session::shutdown(&p, true))),
        ["allow", host, ref flags @ ..] => done(here().and_then(|c| {
            let (scope, agent) = options(flags)?;
            approve::allow(&c, host, scope, agent, &paths()?)
        })),
        ["revoke", "--global", host] | ["revoke", host, "--global"] => {
            done(here().and_then(|c| approve::revoke(&c, host, true, None, &paths()?)))
        }
        ["revoke", host, ref flags @ ..] => done(here().and_then(|c| {
            let (_, agent) = options(flags)?;
            approve::revoke(&c, host, false, agent, &paths()?)
        })),
        ["denied"] => done(here().and_then(|c| approve::denied(&c, &paths()?))),
        ["policy", "show"] => done(here().and_then(|c| approve::show(&c, &paths()?))),
        [agent, ref command @ ..] if agents::Agent::parse(agent).is_some() => {
            here().and_then(|c| run(agent, &c, &args[1..1 + command.len()]))
        }
        _ => Err(format!(
            "usage: agent {} [COMMAND [ARGS...]] | restart | shutdown [--force] | allow HOST[:PORT] [--session|--project] [--agent NAME] | revoke HOST[:PORT] [--agent NAME | --global] | denied | policy show",
            names.join("|")
        )),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("agent-run: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The agent's CLI, or `command` instead, in a session in the checkout; its exit status.
fn run(agent: &str, checkout: &std::path::Path, command: &[String]) -> Result<ExitCode, String> {
    let command = (!command.is_empty()).then_some(command);
    let session = session::prepare(agent, checkout, command, Paths::from_env(agent)?)?;
    if agent::signals::stopped() {
        session.end();
        return Ok(ExitCode::from(143));
    }
    let code = session.run()?;
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}

/// `--session`, `--project` and `--agent NAME`, in any order.
fn options<'a>(flags: &[&'a str]) -> Result<(Scope, Option<&'a str>), String> {
    let (mut scope, mut agent) = (Scope::Session, None);
    let mut rest = flags.iter();
    while let Some(flag) = rest.next() {
        match *flag {
            "--session" => scope = Scope::Session,
            "--project" => scope = Scope::Project,
            "--agent" => agent = Some(*rest.next().ok_or("--agent needs a name")?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok((scope, agent))
}

/// The checkout: always the current folder.
fn checkout() -> Result<PathBuf, String> {
    let path = std::env::current_dir().map_err(|e| format!("current folder: {e}"))?;
    std::fs::canonicalize(&path).map_err(|e| format!("{}: {e}", path.display()))
}
