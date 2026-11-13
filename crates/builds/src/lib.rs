//! The project's build container: started with the pod
//! behind the gate, verified against the pod's plan like an agent's container, serving
//! `gradle-run` on the pod's `localhost` with the project's Gradle home, SDK and build state.

use mountplan::{Plan, verify_and_release};
use pod::Project;
use podman::{Object, Podman};
use std::fs;
use std::path::{Path, PathBuf};

const GRADLE_HOME: &str = "/home/build/.gradle";
const ANDROID_HOME: &str = "/android";
/// The build state volume's subpath in every container of the pod.
pub const BUILD_STATE: &str = "/build-state";

/// The JVM's proxy settings; Java ignores the proxy variables. The mirror is reached directly
/// (the Android SDK library ignores `nonProxyHosts`; the proxy passes its reads).
const JAVA_PROXY: &str = "-Dhttp.proxyHost=proxy -Dhttp.proxyPort=3128 -Dhttps.proxyHost=proxy -Dhttps.proxyPort=3128 \
     -Dhttp.nonProxyHosts=localhost|127.0.0.1|mirror";

/// What the build container gets from the policy and the checkout.
pub struct Settings<'a> {
    pub image: &'a str,
    pub memory: u64,
    pub cpus: f64,
    pub max_concurrent: u32,
    /// `id=url` for each mirror remote, for the mirror init script.
    pub mirror_remotes: Vec<String>,
    pub mirror_url: &'a str,
    /// `sdk.dir` from `local.properties`, checked.
    pub sdk_dir: Option<&'a Path>,
    /// The host's SDK, for its licenses.
    pub host_sdk: Option<PathBuf>,
    /// Records which of the host's licenses the SDK volume has.
    pub licenses_copied: &'a Path,
}

/// Agents read the build state, read-only.
pub fn agent_mount(project: &Project) -> Vec<String> {
    vec![
        "--mount".into(),
        format!(
            "type=volume,src={},dst={BUILD_STATE},subpath={},ro,relabel=shared",
            project.state_volume(),
            pod::BUILD_STATE
        ),
    ]
}

/// Starts the project's build container unless it runs, and gives the SDK volume the host's
/// current licenses either way.
pub fn ensure(
    podman: &Podman,
    project: &Project,
    plan: &Plan,
    settings: &Settings,
) -> Result<(), String> {
    let name = project.build_container();
    let err = |e: podman::PodmanError| e.to_string();
    let mut running = false;
    if podman.exists(Object::Container, &name).map_err(err)? {
        running = podman
            .run(&[
                "container",
                "inspect",
                "--format",
                "{{.State.Running}}",
                &name,
            ])
            .map_err(err)?
            .trim()
            == "true";
        if !running {
            podman
                .run(&["rm", "--force", "--time", "0", &name])
                .map_err(err)?;
        }
    }
    project
        .volume(podman, &project.gradle_volume())
        .map_err(|e| e.to_string())?;
    project
        .volume(podman, &project.android_volume())
        .map_err(|e| e.to_string())?;
    sync_licenses(podman, project, settings)?;
    if running {
        // Replacing it would stop the project's other sessions' builds and displays.
        if podman.image_differs(&name, settings.image).map_err(err)? {
            eprintln!(
                "agent-run: note: the build container runs an older build image; `agent restart` in the project uses the new one"
            );
        }
        return Ok(());
    }
    podman
        .run(&run_args(project, plan, settings))
        .map_err(err)?;
    verify_and_release(podman, &name, plan).map_err(|e| {
        let _ = podman.run(&["rm", "--force", "--time", "0", &name]);
        format!("build container: {e}")
    })
}

fn run_args(project: &Project, plan: &Plan, settings: &Settings) -> Vec<String> {
    let checkout = project.checkout.display().to_string();
    let mut args: Vec<String> = [
        "run",
        "-d",
        "--rm",
        "--pod",
        &project.pod(),
        "--name",
        &project.build_container(),
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
            "8192",
        ]
        .map(String::from),
    );
    args.extend([
        "--memory".into(),
        settings.memory.to_string(),
        "--cpus".into(),
        settings.cpus.to_string(),
        "--workdir".into(),
        checkout.clone(),
    ]);
    args.extend([
        "-v".into(),
        format!("{}:{GRADLE_HOME}:z", project.gradle_volume()),
    ]);
    args.extend([
        "-v".into(),
        format!("{}:{ANDROID_HOME}:z", project.android_volume()),
    ]);
    if let Some(sdk_dir) = settings.sdk_dir {
        // local.properties' sdk.dir wins over ANDROID_HOME: the SDK is there too.
        args.extend([
            "-v".into(),
            format!("{}:{}:z", project.android_volume(), sdk_dir.display()),
        ]);
    }
    args.extend([
        "--mount".into(),
        format!(
            "type=volume,src={},dst={BUILD_STATE},subpath={},relabel=shared",
            project.state_volume(),
            pod::BUILD_STATE
        ),
    ]);
    for (key, value) in [
        ("HOME", "/home/build".to_owned()),
        ("GRADLE_USER_HOME", GRADLE_HOME.to_owned()),
        ("ANDROID_HOME", ANDROID_HOME.to_owned()),
        ("AGENT_RUN_WORKSPACE", checkout),
        ("AGENT_RUN_BUILD_STATE", BUILD_STATE.to_owned()),
        ("AGENT_RUN_MIRROR", settings.mirror_url.to_owned()),
        (
            "AGENT_RUN_MIRROR_REMOTES",
            settings.mirror_remotes.join(","),
        ),
        (
            "AGENT_RUN_MAX_CONCURRENT",
            settings.max_concurrent.to_string(),
        ),
        ("JAVA_TOOL_OPTIONS", JAVA_PROXY.to_owned()),
        ("HTTPS_PROXY", "http://proxy:3128".to_owned()),
        ("NO_PROXY", "localhost,127.0.0.1,mirror".to_owned()),
    ] {
        args.extend(["-e".into(), format!("{key}={value}")]);
    }
    args.extend(plan.podman_args());
    args.extend([
        settings.image.to_owned(),
        "run".into(),
        "--".into(),
        "/usr/local/bin/gradle-run-server".into(),
    ]);
    args
}

/// The new SDK volume gets the host SDK's licenses, without which the Android Gradle Plugin
/// installs nothing.
/// Copies the host's SDK licenses into the SDK volume when they changed since the last copy
/// (or it failed): accepting a license on the Mac reaches builds on the next start. Licenses
/// are only added or updated, never removed; the host's stay as they are.
pub fn sync_licenses(
    podman: &Podman,
    project: &Project,
    settings: &Settings,
) -> Result<(), String> {
    let Some(licenses) = settings
        .host_sdk
        .as_ref()
        .map(|sdk| sdk.join("licenses"))
        .filter(|l| l.is_dir())
    else {
        eprintln!(
            "agent-run: note: no Android SDK licenses on this Mac; builds that need SDK packages will fail until the SDK volume has them"
        );
        return Ok(());
    };
    // The volume by name and creation, so a recreated volume gets them again.
    let created = podman
        .run(&[
            "volume",
            "inspect",
            "--format",
            "{{.CreatedAt}}",
            &project.android_volume(),
        ])
        .map_err(|e| e.to_string())?;
    let current = format!(
        "{} {}\n{}",
        project.android_volume(),
        created.trim(),
        licenses_digest(&licenses)?
    );
    if fs::read_to_string(settings.licenses_copied).ok().as_deref() == Some(current.as_str()) {
        return Ok(());
    }
    // As the pod's user, the volume's first user, so it owns it.
    let target = format!("{ANDROID_HOME}/licenses");
    podman
        .run(&[
            "run",
            "--rm",
            "--network",
            "none",
            "--userns",
            "keep-id:uid=1000,gid=1000",
            "-v",
            &format!("{}:{ANDROID_HOME}", project.android_volume()),
            "-v",
            &format!("{}:/licenses:ro", licenses.display()),
            "--entrypoint",
            "sh",
            settings.image,
            "-c",
            &format!("mkdir -p {target} && cp -R /licenses/. {target}/"),
        ])
        .map_err(|e| e.to_string())?;
    fs::write(settings.licenses_copied, current)
        .map_err(|e| format!("{}: {e}", settings.licenses_copied.display()))
}

/// The license files' names and contents, so any change shows.
fn licenses_digest(licenses: &Path) -> Result<String, String> {
    let mut entries: Vec<_> = fs::read_dir(licenses)
        .map_err(|e| format!("{}: {e}", licenses.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    let mut digest = String::new();
    for path in entries.iter().filter(|p| p.is_file()) {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        digest += &format!(
            "{}={}\n",
            path.file_name().unwrap_or_default().to_string_lossy(),
            text.trim()
        );
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sdk_is_also_where_local_properties_says() {
        let project = Project::new(&podman::Namespace::default(), Path::new("/Users/me/app"));
        let plan = Plan {
            checkout: project.checkout.clone(),
            volume: project.state_volume(),
            mounts: vec![],
        };
        let sdk = PathBuf::from("/Users/me/Library/Android/sdk");
        let settings = Settings {
            image: "img",
            memory: 1,
            cpus: 8.0,
            max_concurrent: 1,
            mirror_remotes: vec!["central=https://repo.maven.apache.org/maven2".into()],
            mirror_url: "http://mirror:8080",
            sdk_dir: Some(&sdk),
            host_sdk: None,
            licenses_copied: Path::new("/nonexistent"),
        };
        let args = run_args(&project, &plan, &settings).join(" ");
        assert!(
            args.contains(&format!(
                "-v {}:/Users/me/Library/Android/sdk:z",
                project.android_volume()
            )),
            "{args}"
        );
        assert!(
            args.contains("AGENT_RUN_MIRROR_REMOTES=central=https://repo.maven.apache.org/maven2")
        );
        assert!(args.ends_with("img run -- /usr/local/bin/gradle-run-server"));
        assert!(args.contains("--cpus 8 "), "{args}");
    }
}
