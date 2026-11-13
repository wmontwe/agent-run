//! A project's engram service: engram's own MCP server, bound to
//! the project, in a container outside the pod with no network. It mounts the engram repository
//! (engram-core read-only) and never the checkout; agents reach it through `socat` over a socket
//! volume. Saves are recorded there and committed on the host when the session ends, signed by
//! the user.

use pod::Project;
use podman::{Object, Podman};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Where agents and the service find the socket.
const SOCKET_DIR: &str = "/run/engram";
const REQUESTS_DIR: &str = "/run/engram-requests";

/// A project's engram package, its repository and the server that serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Engram {
    /// The engram repository (`engram-work`): the package's grandparent.
    pub repo: PathBuf,
    pub project: String,
    pub package: PathBuf,
}

impl Engram {
    /// The engram setup for a package the policy declares, checked: the repository with
    /// engram-core, and a distribution of the server that is newer than its sources.
    pub fn resolve(package: &Path) -> Result<Engram, String> {
        let name = package
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(format!(
                "engram package {}: not a project folder",
                package.display()
            ));
        }
        let repo = package
            .parent()
            .filter(|p| p.ends_with("projects"))
            .and_then(Path::parent);
        let Some(repo) = repo.map(Path::to_path_buf) else {
            return Err(format!(
                "engram package {}: expected <repository>/projects/<project>",
                package.display()
            ));
        };
        let engram = Engram {
            repo,
            project: name.to_owned(),
            package: package.to_path_buf(),
        };
        // engram's own test for its repository.
        if !engram.repo.join("AGENTS.md").is_file() {
            return Err(format!(
                "{}: not an engram repository (no AGENTS.md next to projects/)",
                engram.repo.display()
            ));
        }
        if !engram.core().join("scripts/engram-save.sh").is_file() {
            return Err(format!(
                "{}: no engram-core with scripts/engram-save.sh",
                engram.repo.display()
            ));
        }
        engram.check_distribution()?;
        Ok(engram)
    }

    fn core(&self) -> PathBuf {
        self.repo.join("engram-core")
    }

    fn server(&self) -> PathBuf {
        self.core()
            .join("mcp-server/build/install/engram-mcp/bin/engram-mcp")
    }

    /// The service runs what engram built on the host; it has no network to build it.
    fn check_distribution(&self) -> Result<(), String> {
        let rebuild = format!(
            "build it on the host: cd {} && ./gradlew installDist",
            self.core().join("mcp-server").display()
        );
        let built = fs::metadata(self.server())
            .and_then(|m| m.modified())
            .map_err(|_| format!("engram's MCP server isn't built; {rebuild}"))?;
        let newest = newest_source(&self.core().join("mcp-server"));
        if newest.is_some_and(|source| source > built) {
            return Err(format!(
                "engram's MCP server is older than its sources; {rebuild}"
            ));
        }
        Ok(())
    }

    /// Starts the project's service unless it runs. `requests` is the host folder where its
    /// saves are recorded.
    pub fn ensure(
        &self,
        podman: &Podman,
        project: &Project,
        image: &str,
        requests: &Path,
    ) -> Result<(), String> {
        let err = |e: podman::PodmanError| e.to_string();
        let name = project.engram_container();
        if podman.exists(Object::Container, &name).map_err(err)? {
            if podman
                .run(&[
                    "container",
                    "inspect",
                    "--format",
                    "{{.State.Running}}",
                    &name,
                ])
                .map_err(err)?
                .trim()
                == "true"
            {
                // Replacing it would cut the project's other sessions off their engram.
                if podman.image_differs(&name, image).map_err(err)? {
                    eprintln!(
                        "agent-run: note: the engram service runs an older build image; `agent restart` in the project uses the new one"
                    );
                }
                return Ok(());
            }
            podman
                .run(&["rm", "--force", "--time", "0", &name])
                .map_err(err)?;
        }
        project
            .volume(podman, &project.engram_volume())
            .map_err(|e| e.to_string())?;
        fs::create_dir_all(requests).map_err(|e| format!("{}: {e}", requests.display()))?;
        podman
            .run(&self.service_args(project, image, requests))
            .map(|_| ())
            .map_err(err)
    }

    fn service_args(&self, project: &Project, image: &str, requests: &Path) -> Vec<String> {
        let (repo, core) = (
            self.repo.display().to_string(),
            self.core().display().to_string(),
        );
        let serve = format!("EXEC:{} {},nofork", self.server().display(), self.project);
        let mut args: Vec<String> = [
            "run",
            "-d",
            "--rm",
            "--name",
            &project.engram_container(),
            "--label",
            &project.namespace.label(),
        ]
        .map(String::from)
        .into();
        // No network at all, and the pod's user mapping: what it writes belongs to the user.
        args.extend(
            ["--network", "none", "--userns", "keep-id:uid=1000,gid=1000"].map(String::from),
        );
        args.extend(
            [
                "--read-only",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--memory",
                "1g",
                "--pids-limit",
                "512",
            ]
            .map(String::from),
        );
        args.extend([
            "-v".into(),
            format!("{repo}:{repo}:z"),
            "-v".into(),
            format!("{core}:{core}:ro,z"),
        ]);
        args.extend([
            "-v".into(),
            format!("{}:{SOCKET_DIR}:z", project.engram_volume()),
        ]);
        args.extend([
            "-v".into(),
            format!("{}:{REQUESTS_DIR}:z", requests.display()),
        ]);
        for env in [
            format!("ENGRAM_ROOT={repo}"),
            format!("ENGRAM_SAVE_REQUESTS={REQUESTS_DIR}/requests"),
            "HOME=/tmp".to_owned(),
        ] {
            args.extend(["-e".into(), env]);
        }
        args.extend(["--entrypoint".into(), "socat".into(), image.to_owned()]);
        // One server process per connection: each session has its own.
        args.extend([
            format!("UNIX-LISTEN:{SOCKET_DIR}/mcp.sock,fork,mode=600"),
            serve,
        ]);
        args
    }

    /// What an agent's container gets: the socket, and the package's instructions, skills and
    /// memory read-only at their host paths, so the checkout's engram links resolve.
    pub fn agent_args(&self, project: &Project) -> Vec<String> {
        let mut args = vec![
            "-v".into(),
            format!("{}:{SOCKET_DIR}:z", project.engram_volume()),
        ];
        let template = self.core().join("templates/CLAUDE.local.md");
        // Every entry engram links into checkouts, for every agent; not .mcp.json, whose
        // server is the host's.
        let entries = [
            ".agents",
            ".air",
            ".claude",
            ".codex",
            ".junie",
            ".pi",
            ".memory",
            "AGENTS.md",
        ]
        .map(|e| self.package.join(e));
        for path in entries.iter().chain([&template]).filter(|p| p.exists()) {
            args.extend(["-v".into(), format!("{0}:{0}:ro,z", path.display())]);
        }
        // Skills linked from elsewhere in the engram repository (shared skills, engram-core's):
        // their targets too, read-only, or the links would dangle in the container.
        for target in self.linked_skills() {
            args.extend(["-v".into(), format!("{0}:{0}:ro,z", target.display())]);
        }
        args
    }

    /// The targets of the package's skill links that lead outside the package but stay in the
    /// engram repository; nothing else is followed.
    fn linked_skills(&self) -> Vec<PathBuf> {
        let (Ok(repo), Ok(package)) = (
            fs::canonicalize(&self.repo),
            fs::canonicalize(&self.package),
        ) else {
            return Vec::new();
        };
        let mut targets = Vec::new();
        for tool in [".agents", ".air", ".claude", ".codex", ".junie", ".pi"] {
            for entry in fs::read_dir(package.join(tool).join("skills"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let is_link = entry.file_type().is_ok_and(|t| t.is_symlink());
                if let Ok(target) = fs::canonicalize(entry.path())
                    && is_link
                    && target.starts_with(&repo)
                    && !target.starts_with(&package)
                    && !targets.contains(&target)
                {
                    targets.push(target);
                }
            }
        }
        targets.sort();
        targets
    }

    /// The bridge to the service, as the agent starts it (stdio): command and arguments.
    pub fn bridge() -> (String, Vec<String>) {
        (
            "socat".into(),
            vec!["-".into(), format!("UNIX-CONNECT:{SOCKET_DIR}/mcp.sock")],
        )
    }

    /// For agents that don't read `CLAUDE.local.md` by themselves: engram's entry point, as its
    /// template tells Claude.
    pub const INSTRUCTIONS: &str = "Read `AGENTS.local.md` in the project root as well: it holds this project's Engram context.";

    /// Commits the context on the host if a session asked to save since the last commit:
    /// engram's own save, signed as the user's commits are; its output is captured, not shown,
    /// since the agent's interface may be on the terminal. Returns the commit's summary.
    ///
    /// The service only appends to `requests/requests`; `requests/done` holds how many bytes
    /// are committed, so a request appended during a commit is never lost. A lock of its own
    /// keeps sessions of the project from committing the same request twice, without blocking
    /// session starts while a passphrase prompt waits.
    pub fn save(&self, requests: &Path) -> Result<Option<String>, String> {
        let lock_path = requests.join("save.lock");
        let lock =
            fs::File::create(&lock_path).map_err(|e| format!("{}: {e}", lock_path.display()))?;
        lock.lock()
            .map_err(|e| format!("{}: {e}", lock_path.display()))?;
        let file = requests.join("requests");
        let done_file = requests.join("done");
        let all = fs::read(&file).unwrap_or_default();
        let done: usize = fs::read_to_string(&done_file)
            .ok()
            .and_then(|d| d.trim().parse().ok())
            .unwrap_or(0)
            .min(all.len());
        // Only whole lines: a request being appended right now waits for the next round.
        let end = all[done..]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(done, |i| done + i + 1);
        let new = String::from_utf8_lossy(&all[done..end]);
        let Some(message) = commit_message(&new) else {
            return Ok(None);
        };
        let output = Command::new(self.core().join("scripts/engram-save.sh"))
            .args([&self.project, &message])
            .current_dir(&self.repo)
            .env("ENGRAM_ROOT", &self.repo)
            .output()
            .map_err(|e| format!("engram save: {e}"))?;
        if !output.status.success() {
            let why = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(format!(
                "engram save for {} failed ({why}); its requests stay in {}",
                self.project,
                file.display()
            ));
        }
        fs::write(&done_file, end.to_string())
            .map_err(|e| format!("{}: {e}", done_file.display()))?;
        Ok(message.lines().next().map(str::to_owned))
    }

    /// Removes the project's service; with its pod.
    pub fn remove(podman: &Podman, project: &Project) {
        let _ = podman.run(&["rm", "--force", "--time", "5", &project.engram_container()]);
    }
}

/// The commit message for recorded requests (one per line, newlines escaped): a single
/// request as it is; several with their summaries joined into the subject and each request below.
fn commit_message(recorded: &str) -> Option<String> {
    let requests: Vec<String> = recorded
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(unescape)
        .collect();
    match requests.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        many => {
            let summaries: Vec<&str> = many
                .iter()
                .map(|r| r.lines().next().unwrap_or(""))
                .collect();
            let mut subject = summaries.join("; ");
            if subject.chars().count() > 100 {
                subject = subject.chars().take(99).collect::<String>() + "…";
            }
            Some(format!("{subject}\n\n{}", many.join("\n\n")))
        }
    }
}

/// Undoes the service's escaping: `\\n` is a newline, `\\\\` a backslash.
fn unescape(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (c, c == '\\') {
            (_, true) => match chars.next() {
                Some('n') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            },
            (c, false) => out.push(c),
        }
    }
    out
}

fn newest_source(folder: &Path) -> Option<SystemTime> {
    let mut newest = None;
    let mut folders = vec![folder.join("src"), folder.join("gradle")];
    let files = ["build.gradle.kts", "settings.gradle.kts"].map(|f| folder.join(f));
    let mut consider = |path: &Path| {
        if let Ok(time) = fs::metadata(path).and_then(|m| m.modified()) {
            newest = Some(newest.map_or(time, |n: SystemTime| n.max(time)));
        }
    };
    files.iter().for_each(|f| consider(f));
    while let Some(dir) = folders.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path)
            } else {
                consider(&path)
            }
        }
    }
    newest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(test: &str) -> PathBuf {
        let root = std::env::temp_dir()
            .join(format!("engram-test-{}-{test}", std::process::id()))
            .join("engram-work");
        let server = root.join("engram-core/mcp-server");
        fs::create_dir_all(server.join("src")).unwrap();
        fs::write(server.join("src/Main.kt"), "").unwrap();
        fs::create_dir_all(root.join("engram-core/scripts")).unwrap();
        fs::write(root.join("engram-core/scripts/engram-save.sh"), "").unwrap();
        fs::create_dir_all(root.join("projects/app/.memory")).unwrap();
        fs::write(root.join("AGENTS.md"), "").unwrap();
        fs::write(root.join("projects/app/AGENTS.md"), "").unwrap();
        root
    }

    #[test]
    fn needs_a_package_in_projects_and_a_current_distribution() {
        let root = repo("resolve");
        assert!(
            Engram::resolve(&root.join("projects/app"))
                .unwrap_err()
                .contains("isn't built")
        );
        let bin = root.join("engram-core/mcp-server/build/install/engram-mcp/bin");
        fs::create_dir_all(&bin).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(bin.join("engram-mcp"), "").unwrap();
        let engram = Engram::resolve(&root.join("projects/app")).unwrap();
        assert_eq!(
            (engram.repo.clone(), engram.project.as_str()),
            (root.clone(), "app")
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(root.join("engram-core/mcp-server/src/Main.kt"), "changed").unwrap();
        assert!(
            Engram::resolve(&root.join("projects/app"))
                .unwrap_err()
                .contains("older than its sources")
        );
        assert!(
            Engram::resolve(&root.join("app"))
                .unwrap_err()
                .contains("projects/<project>")
        );
        let project = Project::new(&podman::Namespace::default(), Path::new("/x/app"));
        let args = engram.agent_args(&project).join(" ");
        assert!(
            args.contains(&format!(
                "{0}:{0}:ro,z",
                root.join("projects/app/.memory").display()
            )),
            "{args}"
        );
        assert!(!args.contains(".claude"), "only what exists");
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn requests_become_one_commit_message() {
        assert_eq!(commit_message(""), None);
        assert_eq!(
            commit_message("record x\\n\\nbecause y\\\\n\n").unwrap(),
            "record x\n\nbecause y\\n"
        );
        let both = commit_message("first\nsecond\\n\\nwhy\n").unwrap();
        assert_eq!(both, "first; second\n\nfirst\n\nsecond\n\nwhy");
    }

    #[test]
    fn linked_skills_outside_the_package_are_mounted_too() {
        let root = repo("skills");
        fs::create_dir_all(root.join(".agents/skills/shared")).unwrap();
        fs::write(root.join(".agents/skills/shared/SKILL.md"), "").unwrap();
        fs::create_dir_all(root.join("projects/app/.agents/skills")).unwrap();
        std::os::unix::fs::symlink(
            "../../../../.agents/skills/shared",
            root.join("projects/app/.agents/skills/shared"),
        )
        .unwrap();
        let engram = Engram {
            repo: root.clone(),
            project: "app".into(),
            package: root.join("projects/app"),
        };
        let project = Project::new(&podman::Namespace::default(), Path::new("/x/app"));
        let target = fs::canonicalize(root.join(".agents/skills/shared")).unwrap();
        let args = engram.agent_args(&project).join(" ");
        assert!(
            args.contains(&format!("{0}:{0}:ro,z", target.display())),
            "{args}"
        );
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn saves_only_what_was_requested() {
        let root = repo("save");
        let engram = Engram {
            repo: root.clone(),
            project: "app".into(),
            package: root.join("projects/app"),
        };
        let requests = root.join("requests");
        fs::create_dir_all(&requests).unwrap();
        assert_eq!(engram.save(&requests).unwrap(), None);
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }
}
