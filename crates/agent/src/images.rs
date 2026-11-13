//! Builds agent-run's images when they're missing or their inputs changed. Each image is
//! labelled with a hash of the files it's built from.

use podman::{Object, Podman};
use sha2::{Digest, Sha256};
use std::path::Path;

const INPUTS: &str = "agent-run.inputs";

struct Image<'a> {
    name: String,
    /// Files whose content decides the image, relative to the repository.
    inputs: Vec<String>,
    build: Vec<&'a str>,
}

pub fn agent(agent: &str) -> String {
    format!("localhost/agent-run-{agent}")
}

pub const PROXY: &str = "localhost/agent-run-proxy";
pub const RELAY: &str = "localhost/agent-run-github-relay";
pub const BUILD: &str = "localhost/agent-run-build";
pub const MIRROR: &str = "localhost/agent-run-mirror";

/// A crate's manifest and sources, relative to the repository.
fn crate_files(repository: &Path, name: &str) -> Result<Vec<String>, String> {
    let mut files = vec![format!("crates/{name}/Cargo.toml")];
    let mut folders = vec![repository.join(format!("crates/{name}/src"))];
    while let Some(folder) = folders.pop() {
        let entries =
            std::fs::read_dir(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if let Ok(relative) = path.strip_prefix(repository) {
                files.push(relative.display().to_string());
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Makes sure the agent's, the build's, the proxy's, the relay's and the mirror's images are
/// current.
pub fn ensure(podman: &Podman, repository: &Path, agent_name: &str) -> Result<(), String> {
    let agent_image = agent(agent_name);
    let agent_arg = format!("AGENT={agent_name}");
    // The images' Rust binaries: the gate and gradle-run.
    let rust_inputs = [
        vec!["Cargo.toml".to_owned(), "Cargo.lock".to_owned()],
        crate_files(repository, "gate")?,
        crate_files(repository, "buildrun")?,
    ]
    .concat();
    let images = [
        Image {
            name: agent_image.clone(),
            inputs: [
                vec![
                    "image/Containerfile".to_owned(),
                    "image/Containerfile.ignore".to_owned(),
                    format!("agents/{agent_name}/package.json"),
                    format!("agents/{agent_name}/package-lock.json"),
                ],
                rust_inputs.clone(),
            ]
            .concat(),
            build: vec![
                "--build-arg",
                &agent_arg,
                "-f",
                "image/Containerfile",
                "--ignorefile",
                "image/Containerfile.ignore",
                ".",
            ],
        },
        Image {
            name: BUILD.to_owned(),
            inputs: [
                [
                    "Containerfile",
                    "Containerfile.ignore",
                    "redirect.init.gradle.kts",
                    "mirror.init.gradle.kts",
                    "browser-mcp",
                    "agent-run-display",
                    "browser/package.json",
                    "browser/package-lock.json",
                ]
                .map(|f| format!("build-image/{f}"))
                .to_vec(),
                rust_inputs,
            ]
            .concat(),
            build: vec![
                "-f",
                "build-image/Containerfile",
                "--ignorefile",
                "build-image/Containerfile.ignore",
                ".",
            ],
        },
        Image {
            name: PROXY.to_owned(),
            inputs: vec!["proxy/Dockerfile".to_owned()],
            build: vec!["proxy"],
        },
        Image {
            name: MIRROR.to_owned(),
            inputs: vec!["proxy/mirror/Dockerfile".to_owned()],
            build: vec!["proxy/mirror"],
        },
        Image {
            name: RELAY.to_owned(),
            inputs: ["Dockerfile", "nginx.conf", "start.sh"]
                .map(|f| format!("proxy/github-relay/{f}"))
                .to_vec(),
            build: vec!["proxy/github-relay"],
        },
    ];
    for image in images {
        let hash = inputs_hash(repository, &image.inputs)?;
        let current = podman
            .exists(Object::Image, &image.name)
            .map_err(|e| e.to_string())?
            && podman
                .labels(Object::Image, &image.name)
                .map_err(|e| e.to_string())?
                .get(INPUTS)
                == Some(&hash);
        // Current, but built long ago: rebuilt without the layer cache, so its system
        // packages (Chromium, git, curl, ...) get the updates published since.
        let aged = current && age_days(podman, &image.name)? >= MAX_AGE_DAYS;
        if current && !aged {
            continue;
        }
        if aged {
            eprintln!(
                "agent-run: rebuilding {} (over {MAX_AGE_DAYS} days old: its system packages get their updates)",
                image.name
            );
        } else {
            eprintln!(
                "agent-run: building {} (once, or after its sources change)",
                image.name
            );
        }
        let label = format!("{INPUTS}={hash}");
        let mut args = vec!["build", "-q", "-t", &image.name, "--label", &label];
        if aged {
            args.push("--no-cache");
        }
        args.extend(&image.build);
        let child = std::process::Command::new("podman")
            .args(&args)
            .current_dir(repository)
            .stdout(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("podman build: {e}"))?;
        let output = podman
            .wait(child)
            .map_err(|e| format!("podman build: {e}"))?;
        let output = output.ok_or_else(|| format!("stopped while building {}", image.name))?;
        if !output.status.success() {
            return Err(format!("building {} failed", image.name));
        }
    }
    Ok(())
}

/// Images are rebuilt after this many days, for their system packages' updates.
const MAX_AGE_DAYS: u64 = 14;

/// Days since the image was built.
fn age_days(podman: &Podman, image: &str) -> Result<u64, String> {
    let created = podman
        .run(&["image", "inspect", "--format", "{{.Created.Unix}}", image])
        .map_err(|e| e.to_string())?;
    let created: u64 = created
        .trim()
        .parse()
        .map_err(|_| format!("podman image inspect {image}: {created}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(now.saturating_sub(created) / 86_400)
}

fn inputs_hash(repository: &Path, inputs: &[String]) -> Result<String, String> {
    let mut hasher = Sha256::new();
    for input in inputs {
        let content = std::fs::read(repository.join(input)).map_err(|e| format!("{input}: {e}"))?;
        hasher.update(input.as_bytes());
        hasher.update([0]);
        hasher.update(&content);
    }
    Ok(hasher
        .finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect())
}
