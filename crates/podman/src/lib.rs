//! Podman process execution. A thin wrapper around the CLI: no API client, no JSON.

mod service;

pub use service::{OnChange, Service};

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodmanError {
    pub command: String,
    pub message: String,
}

impl fmt::Display for PodmanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "podman {}: {}", self.command, self.message)
    }
}

impl std::error::Error for PodmanError {}

/// What `podman <kind> exists` and `inspect` work on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Object {
    Network,
    Pod,
    Volume,
    Container,
    Image,
}

impl Object {
    fn command(self) -> &'static str {
        match self {
            Object::Network => "network",
            Object::Pod => "pod",
            Object::Volume => "volume",
            Object::Container => "container",
            Object::Image => "image",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Podman {
    program: String,
    /// Whether to stop waiting: commands still running then are killed.
    cancel: Option<fn() -> bool>,
}

impl Default for Podman {
    fn default() -> Self {
        Podman {
            program: "podman".to_owned(),
            cancel: None,
        }
    }
}

impl Podman {
    /// The same Podman, whose commands are killed once `cancel` returns true: for work the
    /// user can call off, such as a start. Cleanup uses one without.
    pub fn cancelled_by(&self, cancel: fn() -> bool) -> Podman {
        Podman {
            cancel: Some(cancel),
            ..self.clone()
        }
    }

    /// Waits for a child, collecting its output; None if it was cancelled (and killed).
    pub fn wait(&self, mut child: Child) -> std::io::Result<Option<Output>> {
        let Some(cancel) = self.cancel else {
            return child.wait_with_output().map(Some);
        };
        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut bytes);
                }
                bytes
            })
        };
        let stdout = drain(
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let stderr = drain(
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let mut pause = Duration::from_millis(1);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if cancel() {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
            thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(50));
        };
        Ok(Some(Output {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        }))
    }

    /// Runs `podman ARGS` and returns its standard output, or its error output on failure.
    pub fn run<S: AsRef<str>>(&self, args: &[S]) -> Result<String, PodmanError> {
        let child = Command::new(&self.program)
            .args(args.iter().map(AsRef::as_ref))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| self.error(args, e.to_string()))?;
        let output = self
            .wait(child)
            .map_err(|e| self.error(args, e.to_string()))?
            .ok_or_else(|| self.error(args, "stopped: the launcher was told to stop".into()))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(self.error(
                args,
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ))
        }
    }

    /// Whether a container runs another image than `image` names now (rebuilt since).
    pub fn image_differs(&self, container: &str, image: &str) -> Result<bool, PodmanError> {
        let running = self.run(&["container", "inspect", "--format", "{{.Image}}", container])?;
        let wanted = self.run(&["image", "inspect", "--format", "{{.Id}}", image])?;
        Ok(running.trim() != wanted.trim())
    }

    pub fn exists(&self, object: Object, name: &str) -> Result<bool, PodmanError> {
        let args = [object.command(), "exists", name];
        let status = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| self.error(&args, e.to_string()))?;
        // `exists` exits 1 for "no", anything else for an error.
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(self.error(&args, format!("failed with {status}"))),
        }
    }

    /// The labels of a network, pod, volume or container.
    pub fn labels(
        &self,
        object: Object,
        name: &str,
    ) -> Result<BTreeMap<String, String>, PodmanError> {
        // A container keeps them in its configuration; the other objects at the top.
        let labels = if object == Object::Container {
            ".Config.Labels"
        } else {
            ".Labels"
        };
        let format = format!(
            "{{{{range $key, $value := {labels}}}}}{{{{$key}}}}={{{{$value}}}}\n{{{{end}}}}"
        );
        let text = self.run(&[object.command(), "inspect", "--format", &format, name])?;
        Ok(text
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect())
    }

    /// Whether Podman runs rootless: rootful Podman ignores `--userns keep-id`.
    pub fn is_rootless(&self) -> Result<bool, PodmanError> {
        Ok(self
            .run(&["info", "--format", "{{.Host.Security.Rootless}}"])?
            .trim()
            == "true")
    }

    fn error<S: AsRef<str>>(&self, args: &[S], message: String) -> PodmanError {
        let command = args
            .iter()
            .take(2)
            .map(AsRef::as_ref)
            .collect::<Vec<_>>()
            .join(" ");
        PodmanError { command, message }
    }
}

/// Where agent-run's Podman objects live: every name starts with the namespace's prefix and
/// every object carries its label. `agent-run` normally; tests use their own, so they never
/// touch real objects and can remove theirs at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace(String);

pub const NAMESPACE_LABEL: &str = "agent-run.namespace";

impl Default for Namespace {
    fn default() -> Self {
        Namespace("agent-run".to_owned())
    }
}

impl Namespace {
    pub fn new(prefix: &str) -> Namespace {
        Namespace(prefix.to_owned())
    }

    pub fn prefix(&self) -> &str {
        &self.0
    }

    /// `<prefix>-<what>`.
    pub fn name(&self, what: &str) -> String {
        format!("{}-{what}", self.0)
    }

    /// The label every object in the namespace gets, `key=value`.
    pub fn label(&self) -> String {
        format!("{NAMESPACE_LABEL}={}", self.0)
    }

    /// Removes every pod, container and network in the namespace; its volumes stay. Nothing
    /// outside it: objects are found by the namespace's label.
    pub fn stop_all(&self, podman: &Podman) -> Result<(), PodmanError> {
        let filter = format!("label={}", self.label());
        let list = |args: &[&str]| {
            podman
                .run(args)
                .map(|out| out.lines().map(str::to_owned).collect::<Vec<_>>())
        };
        for pod in list(&["pod", "ls", "-q", "--filter", &filter])? {
            podman.run(&["pod", "rm", "--force", "--time", "5", &pod])?;
        }
        // --depend: also the containers that share one's network, whichever comes first.
        for container in list(&["ps", "-aq", "--filter", &filter])? {
            podman.run(&["rm", "--force", "--depend", "--time", "5", &container])?;
        }
        for network in list(&["network", "ls", "-q", "--filter", &filter])? {
            podman.run(&["network", "rm", "--force", &network])?;
        }
        Ok(())
    }

    /// Removes everything in the namespace, volumes too, except volumes whose names end with
    /// one of `keep`. For tests.
    pub fn remove_all(&self, podman: &Podman, keep: &[&str]) {
        let _ = self.stop_all(podman);
        let filter = format!("label={}", self.label());
        let list = |args: &[&str]| {
            podman
                .run(args)
                .map(|out| out.lines().map(str::to_owned).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        for volume in list(&["volume", "ls", "-q", "--filter", &filter]) {
            if !keep.iter().any(|k| volume.ends_with(k)) {
                let _ = podman.run(&["volume", "rm", "--force", &volume]);
            }
        }
    }
}
