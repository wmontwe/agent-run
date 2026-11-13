//! `agent policy show`: what the policy grants, globally and for one project.

use crate::{Destination, Policy};
use std::path::Path;

impl Destination {
    /// As the policy writes it: `host`, or `host:port` for other ports than 443.
    pub fn short(&self) -> String {
        match self.port {
            443 => self.host.clone(),
            port => format!("{}:{port}", self.host),
        }
    }
}

fn list(items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().collect();
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

impl Policy {
    /// The grants for every project, then the checkout's own and its `session` approvals.
    pub fn describe(&self, checkout: &Path, session: &[Destination]) -> String {
        let mib = |bytes: u64| format!("{} MiB", bytes >> 20);
        let view = self.for_project(checkout);
        let own: Vec<String> = self
            .projects
            .get(checkout)
            .map(|p| p.allow.iter().map(Destination::short).collect())
            .unwrap_or_default();
        let rows = [
            (
                "reaches",
                list(self.project_allow.iter().map(Destination::short)),
            ),
            ("GitHub, read-only", list(self.github_repos.iter().cloned())),
            ("masked", list(self.workspace.mask.iter().cloned())),
            ("read-only", list(self.workspace.read_only.iter().cloned())),
            ("private", list(self.workspace.private.iter().cloned())),
            ("agent memory", mib(self.agent_memory)),
        ];
        let mut out = String::from("Every project\n");
        for (key, value) in rows {
            out += &format!("  {:<19}{value}\n", format!("{key}:"));
        }
        out += &format!("{}\n", checkout.display());
        let mut project = vec![
            ("also reaches", list(own)),
            ("this session", list(session.iter().map(Destination::short))),
        ];
        project.push(("build memory", mib(view.build_memory)));
        if let Some(engram) = view.engram {
            project.push(("engram", engram.display().to_string()));
        }
        for (key, value) in project {
            out += &format!("  {:<19}{value}\n", format!("{key}:"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use crate::{destination, parse};
    use std::path::Path;

    #[test]
    fn describes_global_project_and_session_grants() {
        let text = "version = 1\n[network.project]\nallow = [\"api.anthropic.com\"]\n[projects.\"/work/tmc\"]\nnetwork.project.allow = [\"example.org:8443\"]\n";
        let policy = parse(text, Path::new("/Users/me")).unwrap();
        let text = policy.describe(
            Path::new("/work/tmc"),
            &[destination("example.net").unwrap()],
        );
        assert!(
            text.contains("  reaches:           api.anthropic.com\n"),
            "{text}"
        );
        assert!(
            text.contains("  also reaches:      example.org:8443\n"),
            "{text}"
        );
        assert!(
            text.contains("  this session:      example.net\n"),
            "{text}"
        );
        assert!(text.contains("  agent memory:      2048 MiB\n"), "{text}");
    }
}
