//! Kotlin Toolchain wrapper selection and isolated command environment.
//! Contracts inspected at upstream 7133c358a9605541d37d144d73f5860df5be6965.
//! Output redirection is a default, not a sandbox around project/plugin code.

use crate::server::checked_cwd;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The nearest project wrapper, without falling back to a global tool or another checkout.
pub fn wrapper_root(workspace: &Path, cwd: &Path) -> Result<PathBuf, String> {
    let (workspace, cwd) = checked_cwd(workspace, cwd)?;
    find_root(&workspace, &cwd)
}

fn find_root(workspace: &Path, cwd: &Path) -> Result<PathBuf, String> {
    for dir in cwd.ancestors() {
        let project = dir.join("project.yaml").is_file();
        let module = dir.join("module.yaml").is_file();
        let wrapper = dir.join("kotlin");
        let executable = fs::metadata(&wrapper)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
        if executable && (project || module) {
            let resolved = wrapper
                .canonicalize()
                .map_err(|e| format!("{}: {e}", wrapper.display()))?;
            if !resolved.starts_with(workspace) {
                return Err(format!(
                    "{} resolves outside the checkout",
                    wrapper.display()
                ));
            }
            check_version(&resolved)?;
            return Ok(dir.to_path_buf());
        }
        // A nested multi-module project without its own wrapper must not use an outer version.
        if project || dir == workspace {
            return Err(format!(
                "{}: no executable kotlin wrapper alongside project.yaml or module.yaml; add the project's wrapper",
                dir.display()
            ));
        }
    }
    unreachable!("validated cwd has the checkout as an ancestor")
}

/// The oldest Kotlin Toolchain that lets every download address be replaced: older ones download
/// from fixed hosts, which no mirror serves.
const MINIMUM: (u32, u32, u32) = (0, 14, 0);

/// Refuses a wrapper that pins a Kotlin Toolchain older than [`MINIMUM`].
fn check_version(wrapper: &Path) -> Result<(), String> {
    let text = fs::read_to_string(wrapper).map_err(|e| format!("{}: {e}", wrapper.display()))?;
    let version = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("kotlin_cli_version="))
        .ok_or_else(|| format!("{}: no kotlin_cli_version", wrapper.display()))?;
    // `0.14.0`, or a build such as `0.15.0-dev-4640`.
    let mut numbers = version
        .split(['.', '-'])
        .take(3)
        .map(|n| n.parse::<u32>().unwrap_or(0));
    let found = (
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
    );
    if found < MINIMUM {
        let (a, b, c) = MINIMUM;
        return Err(format!(
            "{} pins Kotlin Toolchain {version}; agent-run needs {a}.{b}.{c} or newer, whose \
             downloads can come through the mirror. Update the project's wrapper on the host (`./kotlin update`).",
            wrapper.display()
        ));
    }
    Ok(())
}

/// Kotlin Toolchain's own download settings, each with the upstream addresses it replaces and
/// what follows the address. Each points at the mirror remote of that upstream, as Gradle's
/// repositories do: Kotlin Toolchain can't use the proxy.
const DOWNLOADS: &[(&str, &[&str], &str)] = &[
    ("KOTLIN_CLI_DOWNLOAD_ROOT", KOTLIN_REPOSITORY, ""),
    ("KOTLIN_TOOLCHAIN_REPOSITORY_URL", KOTLIN_REPOSITORY, ""),
    (
        "KOTLIN_DEFAULT_MAVEN_CENTRAL_URL",
        &[
            "https://repo.maven.apache.org/maven2",
            "https://repo1.maven.org/maven2",
        ],
        "",
    ),
    (
        "KOTLIN_TOOLCHAIN_GOOGLE_REPOSITORY_URL",
        &[
            "https://dl.google.com/dl/android/maven2",
            "https://maven.google.com",
        ],
        "",
    ),
    (
        "KOTLIN_TOOLCHAIN_JETBRAINS_DOWNLOAD_URL",
        &["https://download.jetbrains.com"],
        "",
    ),
    (
        "KOTLIN_TOOLCHAIN_ANDROID_DOWNLOAD_URL_ROOT",
        &["https://dl.google.com"],
        "",
    ),
    (
        "KOTLIN_TOOLCHAIN_NODE_JS_DIST_URL",
        &["https://nodejs.org/dist"],
        "",
    ),
    (
        "KOTLIN_TOOLCHAIN_PNPM_DOWNLOAD_ROOT_URL",
        &["https://github.com/pnpm/pnpm/releases/download"],
        "",
    ),
    (
        "KOTLIN_TOOLCHAIN_GRADLE_DISTRIBUTION_URL",
        &["https://services.gradle.org/distributions"],
        "/gradle-{{version}}-bin.zip",
    ),
];

/// The Kotlin Toolchain repository: JetBrains' caching proxy of it first, which serves large
/// files itself; the repository redirects them to a CDN host that changes.
const KOTLIN_REPOSITORY: &[&str] = &[
    "https://artifacts-caching-proxy.aws.intellij.net/packages.jetbrains.team/maven/p/amper/amper",
    "https://packages.jetbrains.team/maven/p/amper/amper",
];

/// Kotlin Toolchain's download settings for the mirror at `mirror` with `remotes` (`(id, url)`),
/// and the upstreams no remote serves: Kotlin Toolchain can't download from those.
pub fn downloads(
    mirror: &str,
    remotes: &[(String, String)],
) -> (Vec<(&'static str, String)>, Vec<&'static str>) {
    let mirror = mirror.trim_end_matches('/');
    let (mut env, mut missing) = (Vec::new(), Vec::new());
    for (key, upstreams, suffix) in DOWNLOADS {
        let remote = remotes
            .iter()
            .find(|(_, url)| upstreams.contains(&url.trim_end_matches('/')));
        match remote {
            Some((id, _)) => env.push((*key, format!("{mirror}/{id}{suffix}"))),
            None if !missing.contains(&upstreams[0]) => missing.push(upstreams[0]),
            None => {}
        }
    }
    (env, missing)
}

/// The image's Temurin 25: the Kotlin CLI's own runtime (instead of the Zulu JRE the launcher
/// would download from Azul), and `JAVA_HOME`, which Kotlin Toolchain uses for a project when it
/// matches the project's JDK criteria. A project needing another JDK would download one from
/// its vendor's host, which no mirror serves.
pub const JDK: &str = "/opt/jdk/25";

/// Prepares a command without running it or provisioning any tools.
/// The caller still owns stdin, output streaming, cancellation and mount verification.
/// In particular this is not an MCP stdio bridge, nor proof of downloader proxy compatibility.
pub fn command(
    workspace: &Path,
    build_state: &Path,
    cwd: &Path,
    args: &[String],
    downloads: &[(&str, String)],
) -> Result<Command, String> {
    let (workspace, cwd) = checked_cwd(workspace, cwd)?;
    let root = find_root(&workspace, &cwd)?;
    let relative = root
        .strip_prefix(&workspace)
        .expect("validated wrapper root");
    let state = build_state.join("kotlin-toolchain");
    let mut command = Command::new(root.join("kotlin"));
    command
        .args(args)
        .current_dir(cwd)
        .env("KOTLIN_CLI_BOOTSTRAP_CACHE_DIR", state.join("bootstrap"))
        .env("KOTLIN_SHARED_CACHE_DIR", state.join("shared"))
        .env(
            "KOTLIN_TOOLCHAIN_BUILD_DIR",
            state.join("projects").join(relative).join("build"),
        )
        .env("KOTLIN_CLI_NO_WELCOME_BANNER", "1")
        .env("KOTLIN_TOOLCHAIN_FUS_ENABLED", "false")
        .env("KOTLIN_CLI_JAVA_HOME", JDK)
        // Builds don't use the image JDK: not through variables, nor its prepended PATH entry.
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/cargo/bin",
        );
    for key in [
        "JDK_HOME",
        "JAVA_TOOL_OPTIONS",
        "JDK_JAVA_OPTIONS",
        "_JAVA_OPTIONS",
        "JAVA_OPTS",
        "GRADLE_OPTS",
    ] {
        command.env_remove(key);
    }
    command.env("JAVA_HOME", JDK);
    command.envs(downloads.iter().map(|(key, value)| (*key, value)));
    // The CLI resolves from the mirror over HTTPS: its JVM trusts agent-run's authority.
    command.env("KOTLIN_CLI_JAVA_OPTIONS", crate::trust::java_options());
    // Proxy environment remains inherited from the build container. No anonymous TCP
    // credential bridge: pod-local listeners would also be reachable by agents.
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "kotlin-backend-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn project(&self, relative: &str) -> PathBuf {
            let root = self.0.join(relative);
            fs::create_dir_all(root.join("app/src")).unwrap();
            fs::write(root.join("project.yaml"), "modules: [app]\n").unwrap();
            fs::write(root.join("app/module.yaml"), "product: jvm/app\n").unwrap();
            fs::write(
                root.join("kotlin"),
                "#!/bin/sh\nkotlin_cli_version=0.14.0\nexit 0\n",
            )
            .unwrap();
            fs::set_permissions(root.join("kotlin"), fs::Permissions::from_mode(0o755)).unwrap();
            root
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn finds_module_and_nested_project_wrappers_within_the_checkout() {
        let scratch = Scratch::new();
        let root = scratch.project("checkout");
        assert_eq!(wrapper_root(&root, &root.join("app/src")).unwrap(), root);
        let nested = scratch.project("checkout/tools");
        assert_eq!(
            wrapper_root(&root, &nested.join("app/src")).unwrap(),
            nested
        );
        fs::remove_file(nested.join("project.yaml")).unwrap();
        fs::write(nested.join("module.yaml"), "product: jvm/app\n").unwrap();
        assert_eq!(wrapper_root(&root, &nested).unwrap(), nested);
    }

    #[test]
    fn ignores_a_wrapper_without_project_markers() {
        let scratch = Scratch::new();
        let root = scratch.project("checkout");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::copy(root.join("kotlin"), root.join("scripts/kotlin")).unwrap();
        assert_eq!(wrapper_root(&root, &root.join("scripts")).unwrap(), root);
    }

    #[test]
    fn refuses_missing_nonexecutable_and_escaping_wrappers() {
        let scratch = Scratch::new();
        let root = scratch.project("checkout");
        let nested = scratch.project("checkout/nested");
        fs::set_permissions(nested.join("kotlin"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            wrapper_root(&root, &nested)
                .unwrap_err()
                .contains("no executable kotlin")
        );
        fs::remove_file(nested.join("kotlin")).unwrap();
        assert!(wrapper_root(&root, &nested.join("app/src")).is_err());
        assert!(
            wrapper_root(&nested, &root)
                .unwrap_err()
                .contains("outside the checkout")
        );
        let outside = scratch.project("outside");
        symlink(outside.join("kotlin"), nested.join("kotlin")).unwrap();
        assert!(
            wrapper_root(&root, &nested)
                .unwrap_err()
                .contains("outside the checkout")
        );
    }

    #[test]
    fn executes_the_wrapper_with_plugin_arguments_and_no_image_jvm_selection() {
        let scratch = Scratch::new();
        let root = scratch.project("checkout");
        fs::write(root.join("kotlin"), "#!/bin/sh\nkotlin_cli_version=0.14.0\n[ \"${JAVA_HOME-}\" = /opt/jdk/25 ] || exit 91\n[ \"${KOTLIN_CLI_JAVA_HOME-}\" = /opt/jdk/25 ] || exit 92\n[ -z \"${JAVA_TOOL_OPTIONS+x}\" ] || exit 93\ncase \"$PATH\" in */opt/jdk/*) exit 94 ;; esac\nprintf 'arg=<%s>\\n' \"$@\"\nprintf 'cwd=<%s>\\n' \"$PWD\"\nexit 7\n").unwrap();
        let args = ["do", "customCommand", "--module", "a b", "", "λ\t%"].map(String::from);
        let output = command(
            &root,
            Path::new("/build-state"),
            &root.join("app/src"),
            &args,
            &[],
        )
        .unwrap()
        .output()
        .unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "arg=<do>\narg=<customCommand>\narg=<--module>\narg=<a b>\narg=<>\narg=<λ\t%>\ncwd=<{}>\n",
                root.join("app/src").display()
            )
        );
    }

    #[test]
    fn keeps_plugin_arguments_and_scopes_outputs_by_project_not_cwd() {
        let scratch = Scratch::new();
        let root = scratch.project("checkout");
        let nested = scratch.project("checkout/tools");
        let args: Vec<String> = [
            "do",
            "updateBaseline",
            "--module",
            "api",
            "--module",
            "a b",
            "",
            "λ\t%",
        ]
        .map(String::from)
        .into();
        let cmd = command(
            &root,
            Path::new("/build-state"),
            &nested.join("app/src"),
            &args,
            &downloads(
                "https://mirror:8443",
                &[("kotlin".to_owned(), KOTLIN_REPOSITORY[0].to_owned())],
            )
            .0,
        )
        .unwrap();
        assert_eq!(cmd.get_program(), nested.join("kotlin"));
        assert_eq!(
            cmd.get_current_dir(),
            Some(nested.join("app/src").as_path())
        );
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            args.iter().map(OsStr::new).collect::<Vec<_>>()
        );
        let env: BTreeMap<_, _> = cmd.get_envs().collect();
        assert_eq!(
            env[OsStr::new("KOTLIN_TOOLCHAIN_BUILD_DIR")],
            Some(OsStr::new(
                "/build-state/kotlin-toolchain/projects/tools/build"
            ))
        );
        assert_eq!(
            env[OsStr::new("KOTLIN_SHARED_CACHE_DIR")],
            Some(OsStr::new("/build-state/kotlin-toolchain/shared"))
        );
        assert_eq!(
            env[OsStr::new("KOTLIN_CLI_BOOTSTRAP_CACHE_DIR")],
            Some(OsStr::new("/build-state/kotlin-toolchain/bootstrap"))
        );
        // Android's user folder is ~/.android, in the build container's writable home.
        assert!(!env.contains_key(OsStr::new("ANDROID_USER_HOME")));
        assert_eq!(env[OsStr::new("JAVA_TOOL_OPTIONS")], None);
        assert_eq!(
            env[OsStr::new("JAVA_HOME")],
            Some(OsStr::new("/opt/jdk/25"))
        );
        assert_eq!(
            env[OsStr::new("KOTLIN_CLI_JAVA_OPTIONS")],
            Some(OsStr::new(crate::trust::java_options().as_str()))
        );
        // The CLI runs on the image's Temurin; the distribution comes from one fixed host.
        assert_eq!(
            env[OsStr::new("KOTLIN_CLI_JAVA_HOME")],
            Some(OsStr::new("/opt/jdk/25"))
        );
        assert_eq!(
            env[OsStr::new("KOTLIN_CLI_DOWNLOAD_ROOT")],
            Some(OsStr::new("https://mirror:8443/kotlin"))
        );
        assert!(
            !env[OsStr::new("PATH")]
                .unwrap()
                .to_string_lossy()
                .contains("/opt/jdk")
        );
        assert!(
            !env.contains_key(OsStr::new("HTTPS_PROXY")),
            "proxy settings must not be replaced"
        );
        let from_root = command(&root, Path::new("/build-state"), &nested, &[], &[]).unwrap();
        assert_eq!(
            from_root.get_envs().collect::<BTreeMap<_, _>>()
                [OsStr::new("KOTLIN_TOOLCHAIN_BUILD_DIR")],
            env[OsStr::new("KOTLIN_TOOLCHAIN_BUILD_DIR")]
        );
    }

    #[test]
    fn every_download_goes_to_its_upstreams_mirror_remote() {
        let remotes: Vec<(String, String)> = [
            ("c", "https://repo1.maven.org/maven2/"),
            ("g", "https://maven.google.com"),
            ("k", "https://packages.jetbrains.team/maven/p/amper/amper"),
            ("gd", "https://services.gradle.org/distributions"),
        ]
        .into_iter()
        .map(|(id, url)| (id.to_owned(), url.to_owned()))
        .collect();
        let (env, missing) = downloads("https://mirror:8443/", &remotes);
        let get = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str());
        // Any spelling of an upstream, with or without a trailing slash.
        assert_eq!(
            get("KOTLIN_DEFAULT_MAVEN_CENTRAL_URL"),
            Some("https://mirror:8443/c")
        );
        assert_eq!(
            get("KOTLIN_TOOLCHAIN_GOOGLE_REPOSITORY_URL"),
            Some("https://mirror:8443/g")
        );
        assert_eq!(
            get("KOTLIN_CLI_DOWNLOAD_ROOT"),
            Some("https://mirror:8443/k")
        );
        assert_eq!(
            get("KOTLIN_TOOLCHAIN_REPOSITORY_URL"),
            Some("https://mirror:8443/k")
        );
        assert_eq!(
            get("KOTLIN_TOOLCHAIN_GRADLE_DISTRIBUTION_URL"),
            Some("https://mirror:8443/gd/gradle-{{version}}-bin.zip")
        );
        // No remote, no setting: Kotlin Toolchain would fail on that download, so it's reported.
        assert_eq!(get("KOTLIN_TOOLCHAIN_PNPM_DOWNLOAD_ROOT_URL"), None);
        assert_eq!(
            missing,
            [
                "https://download.jetbrains.com",
                "https://dl.google.com",
                "https://nodejs.org/dist",
                "https://github.com/pnpm/pnpm/releases/download",
            ]
        );
    }

    #[test]
    fn refuses_a_kotlin_toolchain_older_than_0_14() {
        let scratch = Scratch::new();
        let root = scratch.project("checkout");
        let pin = |version: &str| {
            fs::write(
                root.join("kotlin"),
                format!("#!/bin/sh\nkotlin_cli_version={version}\nexit 0\n"),
            )
            .unwrap();
            wrapper_root(&root, &root)
        };
        let old = pin("0.13.0").unwrap_err();
        assert!(old.contains("pins Kotlin Toolchain 0.13.0"), "{old}");
        assert!(old.contains("on the host (`./kotlin update`)"), "{old}");
        assert!(pin("0.14.0").is_ok());
        assert!(pin("0.15.0-dev-4640").is_ok());
        assert!(pin("1.0").is_ok());
    }
}
