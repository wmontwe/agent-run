//! A shared service (the egress proxy, the GitHub relay, the mirror): one long-lived container
//! on its own network, joined to each project pod's network under an alias, with its
//! configuration in a host folder it mounts.

use crate::{Object, Podman, PodmanError};
use std::fs;
use std::path::{Path, PathBuf};

/// What a service does when its configuration changed while it runs.
#[derive(Debug, Clone, Copy)]
pub enum OnChange<'a> {
    /// Run this command in the container.
    Exec(&'a [&'a str]),
    /// Send this signal to its main process.
    Signal(&'a str),
    Restart,
}

pub struct Service<'a> {
    pub podman: &'a Podman,
    pub container: String,
    /// Its own network: the only one with a route out unless `internal`.
    pub network: String,
    pub internal: bool,
    /// Its name on the pods' networks.
    pub alias: &'a str,
    /// Its configuration on the host; a container that doesn't mount it (started with another
    /// agent-run home) is replaced.
    pub config_dir: PathBuf,
    /// The label every object gets (`key=value`), so a namespace can be cleaned up at once.
    pub label: String,
}

fn io(path: &Path, e: std::io::Error) -> PodmanError {
    PodmanError {
        command: "write".into(),
        message: format!("{}: {e}", path.display()),
    }
}

impl Service<'_> {
    /// Writes `files` into the configuration folder, in place (the VM's file share shows a
    /// file renamed over another as missing for seconds). Returns whether they differ from what
    /// the service last accepted: a failed reload stays pending, even with the same files.
    pub fn write_config(&self, files: &[(&str, String)]) -> Result<bool, PodmanError> {
        fs::create_dir_all(&self.config_dir).map_err(|e| io(&self.config_dir, e))?;
        let mut desired = String::new();
        for (name, text) in files {
            let file = self.config_dir.join(name);
            if fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
                fs::write(&file, text).map_err(|e| io(&file, e))?;
            }
            desired += &format!("{name}\0{text}\0");
        }
        fs::write(self.state_file("desired"), &desired)
            .map_err(|e| io(&self.state_file("desired"), e))?;
        Ok(fs::read_to_string(self.state_file("applied"))
            .ok()
            .as_deref()
            != Some(desired.as_str()))
    }

    /// Next to the configuration folder, not in it: the container mounts the folder.
    fn state_file(&self, kind: &str) -> std::path::PathBuf {
        self.config_dir.with_extension(kind)
    }

    /// Records the written configuration as accepted by the service.
    fn applied(&self) -> Result<(), PodmanError> {
        match fs::read(self.state_file("desired")) {
            Ok(desired) => fs::write(self.state_file("applied"), desired)
                .map_err(|e| io(&self.state_file("applied"), e)),
            Err(_) => Ok(()),
        }
    }

    /// Creates the network if needed.
    pub fn network(&self) -> Result<(), PodmanError> {
        if !self.podman.exists(Object::Network, &self.network)? {
            let mut args = vec!["network", "create", "--label", &self.label];
            if self.internal {
                args.push("--internal");
            }
            args.push(&self.network);
            self.podman.run(&args)?;
        }
        Ok(())
    }

    /// Makes sure the service runs: started with `options` (mounts, limits, image and command)
    /// when missing, started when stopped, `on_change` applied when `changed`.
    pub fn ensure(
        &self,
        options: &[String],
        image: &str,
        command: &[String],
        changed: bool,
        on_change: OnChange,
    ) -> Result<(), PodmanError> {
        self.network()?;
        let name = self.container.as_str();
        // The pods' networks it served, with its addresses there, when it's replaced.
        let mut joined = Vec::new();
        if self.podman.exists(Object::Container, name)? {
            let sources = self.podman.run(&[
                "container",
                "inspect",
                "--format",
                "{{range .Mounts}}{{.Source}}\n{{end}}",
                name,
            ])?;
            // Replaced when started with another configuration folder, or when its image was
            // rebuilt since: a running service gets the new image. Its volumes stay.
            if !sources
                .lines()
                .any(|source| Path::new(source) == self.config_dir)
                || self.podman.image_differs(name, image)?
            {
                joined = self
                    .networks()?
                    .into_iter()
                    .filter(|(network, _)| *network != self.network)
                    .collect();
                self.podman.run(&["rm", "--force", "--time", "0", name])?;
            }
        }
        if !self.podman.exists(Object::Container, name)? {
            let mut args: Vec<String> = [
                "run",
                "-d",
                "--name",
                name,
                "--network",
                &self.network,
                "--label",
                &self.label,
            ]
            .map(String::from)
            .into();
            // The services read what untrusted containers send, so they get the same hardening.
            args.extend(
                [
                    "--read-only",
                    "--cap-drop",
                    "ALL",
                    "--security-opt",
                    "no-new-privileges",
                ]
                .map(String::from),
            );
            args.extend(options.iter().cloned());
            args.push(image.to_owned());
            args.extend(command.iter().cloned());
            self.podman.run(&args)?;
            // Every project it served keeps it, at the same address (the mirror has the proxy's).
            for (network, address) in &joined {
                if self.podman.exists(Object::Network, network)? {
                    self.podman.run(&[
                        "network", "connect", "--alias", self.alias, "--ip", address, network, name,
                    ])?;
                }
            }
            return self.applied();
        }
        if self
            .podman
            .run(&[
                "container",
                "inspect",
                "--format",
                "{{.State.Running}}",
                name,
            ])?
            .trim()
            != "true"
        {
            self.podman.run(&["start", name])?;
        } else if changed {
            match on_change {
                OnChange::Exec(command) => {
                    self.podman.run(&[&["exec", name][..], command].concat())?
                }
                OnChange::Signal(signal) => self.podman.run(&["kill", "--signal", signal, name])?,
                OnChange::Restart => self.podman.run(&["restart", "--time", "5", name])?,
            };
        }
        self.applied()
    }

    /// Joins a pod's network, where the pod reaches the service by its alias.
    pub fn connect(&self, network: &str) -> Result<(), PodmanError> {
        if !self.connected(network)? {
            self.podman.run(&[
                "network",
                "connect",
                "--alias",
                self.alias,
                network,
                &self.container,
            ])?;
        }
        Ok(())
    }

    /// Leaves a pod's network, so the network can be removed.
    pub fn disconnect(&self, network: &str) -> Result<(), PodmanError> {
        if self.podman.exists(Object::Container, &self.container)? && self.connected(network)? {
            self.podman
                .run(&["network", "disconnect", network, &self.container])?;
        }
        Ok(())
    }

    fn connected(&self, network: &str) -> Result<bool, PodmanError> {
        Ok(self.networks()?.iter().any(|(n, _)| n == network))
    }

    /// The networks it's on, with its address on each.
    fn networks(&self) -> Result<Vec<(String, String)>, PodmanError> {
        let format = "{{range $name, $net := .NetworkSettings.Networks}}{{$name}} {{$net.IPAddress}}\n{{end}}";
        let listing =
            self.podman
                .run(&["container", "inspect", "--format", format, &self.container])?;
        Ok(listing
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(n, ip)| (n.to_owned(), ip.to_owned()))
            .collect())
    }

    /// The service's IP address on `network`.
    pub fn address_on(&self, network: &str) -> Result<String, PodmanError> {
        let format = format!("{{{{(index .NetworkSettings.Networks \"{network}\").IPAddress}}}}");
        Ok(self
            .podman
            .run(&["container", "inspect", "--format", &format, &self.container])?
            .trim()
            .to_owned())
    }

    /// The service's log.
    pub fn log(&self) -> Result<String, PodmanError> {
        if !self.podman.exists(Object::Container, &self.container)? {
            return Ok(String::new());
        }
        self.podman.run(&["logs", &self.container])
    }
}
