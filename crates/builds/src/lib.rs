//! The project's build container: started with the pod
//! behind the gate, verified against the pod's plan like an agent's container, serving
//! build requests on the pod's `localhost` with the project's caches, SDK and build state.

use mountplan::{Plan, verify_and_release};
use pod::Project;
use podman::{Object, Podman};
use std::fs;
use std::path::{Path, PathBuf};

/// The build user's home, writable (see [`pod::BUILD_HOME`]).
const HOME: &str = "/home/build";
const GRADLE_HOME: &str = "/home/build/.gradle";
const ANDROID_HOME: &str = "/android";
/// The build state volume's subpath in every container of the pod.
pub const BUILD_STATE: &str = "/build-state";
/// Lets reused containers prove they still have the current build proxy credentials.
pub const PROXY_USER_LABEL: &str = "agent-run.build-proxy-user";

/// Added to every agent's global instructions: compilers run in the build container.
pub const INSTRUCTIONS: &str = "This container has no JDK or Gradle. Run Gradle tasks with `gradle-run <tasks>` \
     (e.g. `gradle-run test`), not `./gradlew`: it runs the project's Gradle wrapper in the project's build \
     container (JDKs 11, 17, 21 and 25) and streams the output back. Build results are readable under \
     `/build-state`. This container also has no Rust toolchain. Use `cargo-run <arguments>` \
     (e.g. `cargo-run test`, `cargo-run fmt --all`, `cargo-run clippy --all-targets`) instead of `cargo`: \
     Rust runs in the same build container, with caches under `/build-state/rust/cargo` and target \
     output under `/build-state/rust/targets/<working-directory-relative-to-checkout>`. Only the \
     image's installed Rust toolchains are available; macOS-specific code still needs macOS tests. \
     For Kotlin Toolchain projects (project.yaml/module.yaml and a kotlin wrapper), use \
     `kotlin-run <arguments>` (e.g. `kotlin-run build`, `kotlin-run test`, \
     `kotlin-run show commands`, `kotlin-run do <plugin-command>`), not `./kotlin`: \
     it executes the project's checked-in wrapper in the build container, preserving its version, \
     checksum and command arguments. The wrapper must pin Kotlin Toolchain 0.14.0 or newer \
     (updated on the host with `./kotlin update`); its downloads come through the dependency mirror, and it uses the \
     image's JDK 25. iOS and other Kotlin/Native targets don't build here (no Linux arm64 \
     compiler); don't install a JDK or compiler here as a workaround. The host's Maven local \
     repository isn't shared: when a dependency the project publishes itself (a module with \
     `repositories: - url: mavenLocal` and `publish: true`) doesn't resolve, publish that module \
     here first with `kotlin-run publish mavenLocal -m <module>` (`gradle-run publishToMavenLocal` \
     for Gradle); the build container's Maven local repository is kept between builds. Caches are under \
     `/build-state/kotlin-toolchain/{bootstrap,shared}`; default results are under \
     `/build-state/kotlin-toolchain/projects/<wrapper-root-relative-to-checkout>/build`. \
     Report actual download errors (a missing mirror remote is named in the output); hosts \
     go in the host-side policy. Do not run on the host as a workaround. Builds are noninteractive. \
     `kotlin-run run` holds a build slot until it exits; Compose MCP's bidirectional stdio \
     server is not supported through this foreground build bridge.";

/// The JVM's proxy settings; Java ignores the proxy variables. The mirror is reached directly
/// (the Android SDK library ignores `nonProxyHosts`; the proxy passes its reads).
const JAVA_PROXY: &str = "-Dhttp.proxyHost=proxy -Dhttp.proxyPort=3128 -Dhttps.proxyHost=proxy -Dhttps.proxyPort=3128 \
     -Dhttp.nonProxyHosts=localhost|127.0.0.1|mirror";

/// What the build container gets from the policy and the checkout.
pub struct Settings<'a> {
    pub image: &'a str,
    /// Authenticated build identity: user label and proxy URL, never supplied to agents.
    pub proxy: Option<(&'a str, &'a str)>,
    pub memory: u64,
    pub cpus: f64,
    pub max_concurrent: u32,
    /// `id=url` for each mirror remote, for the mirror init script.
    pub mirror_remotes: Vec<String>,
    pub mirror_url: &'a str,
    /// agent-run's authority for the mirror's HTTPS, on the host.
    pub mirror_ca: &'a Path,
    /// `sdk.dir` from `local.properties`, checked.
    pub sdk_dir: Option<&'a Path>,
    /// The host's SDK, for its licenses.
    pub host_sdk: Option<PathBuf>,
    /// Records which of the host's licenses the SDK volume has.
    pub licenses_copied: &'a Path,
    /// Read-only mounts that make links in the checkout resolve (engram's).
    pub link_mounts: Vec<String>,
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
        if let Some((user, _)) = settings.proxy {
            let labels = podman.labels(Object::Container, &name).map_err(err)?;
            if labels.get(PROXY_USER_LABEL).map(String::as_str) != Some(user) {
                return Err("the build container uses older proxy credentials; run `agent restart` in this project".into());
            }
        }
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
    if let Some((user, _)) = settings.proxy {
        args.extend(["--label".into(), format!("{PROXY_USER_LABEL}={user}")]);
    }
    let proxy = settings
        .proxy
        .map(|(_, url)| url)
        .unwrap_or("http://proxy:3128");
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
    // A writable home, before the Gradle home that's mounted inside it.
    args.extend([
        "--mount".into(),
        format!(
            "type=volume,src={},dst={HOME},subpath={},relabel=shared",
            project.state_volume(),
            pod::BUILD_HOME
        ),
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
        "-v".into(),
        format!(
            "{}:{}:ro,z",
            settings.mirror_ca.display(),
            buildrun::trust::CA
        ),
    ]);
    args.extend([
        "--mount".into(),
        format!(
            "type=volume,src={},dst={BUILD_STATE},subpath={},relabel=shared",
            project.state_volume(),
            pod::BUILD_STATE
        ),
    ]);
    args.extend(settings.link_mounts.iter().cloned());
    for (key, value) in [
        ("HOME", HOME.to_owned()),
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
        ("AGENT_RUN_MIRROR_CA", buildrun::trust::CA.to_owned()),
        (
            "JAVA_TOOL_OPTIONS",
            format!("{JAVA_PROXY} {}", buildrun::trust::java_options()),
        ),
        // The server writes these before it serves (see buildrun::trust).
        ("CURL_CA_BUNDLE", buildrun::trust::BUNDLE.to_owned()),
        ("SSL_CERT_FILE", buildrun::trust::BUNDLE.to_owned()),
        ("NODE_EXTRA_CA_CERTS", buildrun::trust::CA.to_owned()),
        ("HTTPS_PROXY", proxy.to_owned()),
        ("HTTP_PROXY", proxy.to_owned()),
        ("https_proxy", proxy.to_owned()),
        ("http_proxy", proxy.to_owned()),
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
    fn agent_instructions_cover_kotlin_toolchain_and_project_plugins() {
        assert!(INSTRUCTIONS.contains("`kotlin-run <arguments>`"));
        assert!(INSTRUCTIONS.contains("`kotlin-run build`"));
        assert!(INSTRUCTIONS.contains("`kotlin-run do <plugin-command>`"));
        assert!(INSTRUCTIONS.contains("not `./kotlin`"));
        assert!(INSTRUCTIONS.contains("Kotlin Toolchain 0.14.0 or newer"));
        assert!(INSTRUCTIONS.contains("iOS and other Kotlin/Native targets don't build here"));
        assert!(INSTRUCTIONS.contains("`kotlin-run publish mavenLocal -m <module>`"));
    }

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
            proxy: Some(("build-123", "http://build-123:secret@proxy:3128")),
            memory: 1,
            cpus: 8.0,
            max_concurrent: 1,
            mirror_remotes: vec!["central=https://repo.maven.apache.org/maven2".into()],
            mirror_url: "https://mirror:8443",
            mirror_ca: Path::new("/state/mirror-tls/ca.crt"),
            sdk_dir: Some(&sdk),
            host_sdk: None,
            licenses_copied: Path::new("/nonexistent"),
            link_mounts: vec![
                "-v".into(),
                "/engram/AGENTS.md:/engram/AGENTS.md:ro,z".into(),
            ],
        };
        let argv = run_args(&project, &plan, &settings);
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["-v", "/engram/AGENTS.md:/engram/AGENTS.md:ro,z"]),
            "the checkout's links resolve in builds"
        );
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["-e", "HTTPS_PROXY=http://build-123:secret@proxy:3128"])
        );
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--label", "agent-run.build-proxy-user=build-123"])
        );
        let args = argv.join(" ");
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
        // A writable home, mounted before the Gradle home inside it.
        let home = args
            .find("dst=/home/build,subpath=home,")
            .expect("a writable home");
        assert!(
            home < args.find(":/home/build/.gradle:z").unwrap(),
            "{args}"
        );
        // The mirror over HTTPS: its authority mounted, every JVM and curl trusting it.
        assert!(
            args.contains("-v /state/mirror-tls/ca.crt:/etc/agent-run/mirror-ca.crt:ro,z"),
            "{args}"
        );
        let env = |pair: &str| argv.windows(2).any(|w| w[0] == "-e" && w[1] == pair);
        assert!(env("AGENT_RUN_MIRROR=https://mirror:8443"));
        assert!(env("AGENT_RUN_MIRROR_CA=/etc/agent-run/mirror-ca.crt"));
        assert!(env("CURL_CA_BUNDLE=/tmp/agent-run/ca-bundle.pem"));
        assert!(env("NODE_EXTRA_CA_CERTS=/etc/agent-run/mirror-ca.crt"));
        assert!(
            argv.iter()
                .any(|a| a.starts_with("JAVA_TOOL_OPTIONS=-Dhttp.proxyHost=proxy")
                    && a.ends_with("-Djavax.net.ssl.trustStore=/tmp/agent-run/truststore.p12 -Djavax.net.ssl.trustStorePassword=changeit")),
            "{args}"
        );
    }
}
