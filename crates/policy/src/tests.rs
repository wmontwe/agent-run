use super::*;

const HOME: &str = "/Users/me";

fn ok(text: &str) -> Policy {
    parse(text, Path::new(HOME)).unwrap_or_else(|e| panic!("{e}"))
}

fn err(text: &str) -> String {
    parse(text, Path::new(HOME))
        .expect_err("expected an error")
        .0
}

/// User-facing examples must stay valid, including partial configuration sections.
#[test]
fn documented_policy_examples_are_valid() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut examples = 0;
    for name in ["README.md", "docs/configuration.md"] {
        let text = std::fs::read_to_string(repository.join(name)).unwrap();
        let mut sections = text.split("```toml\n");
        sections.next();
        for section in sections {
            let sample = section.split_once("```").expect("closed TOML example").0;
            let sample = if sample.starts_with("version = 1") {
                sample.to_owned()
            } else {
                format!("version = 1\n{sample}")
            };
            parse(&sample, Path::new(HOME))
                .unwrap_or_else(|e| panic!("invalid policy example in {name}: {e}"));
            examples += 1;
        }
    }
    assert!(examples >= 2, "expected README and configuration examples");
}

#[test]
fn defaults_when_sections_are_missing() {
    let policy = ok("version = 1");
    assert!(policy.project_allow.is_empty());
    assert_eq!(policy.workspace, WorkspaceRules::default());
    assert_eq!(
        (
            policy.agent_memory,
            policy.build_memory,
            policy.max_concurrent_builds
        ),
        (2 << 30, 16 << 30, 1)
    );
}

#[test]
fn rejects_unknown_keys_anywhere() {
    assert!(err("version = 1\nverison = 2").contains("verison"));
    assert!(err("version = 1\n[workspace]\nmasks = []").contains("masks"));
    assert!(err("version = 1\n[projects.\"~/p\"]\nengramm = \"~/e\"").contains("engramm"));
    assert!(err("version = 2").contains("version"));
}

#[test]
fn folder_rules_are_exact_relative_folders() {
    for bad in [
        "*.signing",
        "build/**",
        "/abs",
        "trailing/",
        "a/../b",
        "./x",
        "",
        "a//b",
        "a\\b",
    ] {
        let text = format!("version = 1\n[workspace]\nmask = [{bad:?}]");
        assert!(
            err(&text).contains("workspace.mask"),
            "{bad} should be refused"
        );
    }
    assert!(err("version = 1\n[workspace]\nread-only = [\".git\", \".git\"]").contains("twice"));
    assert_eq!(
        ok("version = 1\n[workspace]\nprivate = [\"kotlin-js-store\", \"gradle/wrapper\"]")
            .workspace
            .private
            .len(),
        2
    );
}

#[test]
fn destinations_are_host_names_with_optional_port() {
    let policy = ok(
        "version = 1\n[network.project]\nallow = [\"api.example.com\", \"maven.example.org:8443\"]",
    );
    assert_eq!(
        policy.project_allow[1],
        Destination {
            host: "maven.example.org".into(),
            port: 8443
        }
    );
    for bad in [
        "*.example.com",
        "10.0.0.1",
        "Example.com",
        "localhost",
        "a.b:0",
        "a.b:99999",
        "-a.b",
        "a..b",
        "a.b:",
    ] {
        let text = format!("version = 1\n[network.project]\nallow = [{bad:?}]");
        assert!(
            err(&text).contains("network.project.allow"),
            "{bad} should be refused"
        );
    }
}

#[test]
fn validates_repos_remotes_and_sizes() {
    assert!(err("version = 1\n[network.github]\nrepos = [\"no-slash\"]").contains("owner/repo"));
    assert!(
        err("version = 1\n[mirror.remotes]\ncentral = \"http://repo.example.com/m2\"")
            .contains("https")
    );
    assert!(
        err("version = 1\n[mirror.remotes]\nCentral = \"https://repo.example.com/m2\"")
            .contains("lowercase")
    );
    assert!(err("version = 1\n[agents]\nmemory = \"2GB\"").contains("agents.memory"));
    assert!(err("version = 1\n[builds]\nmax-concurrent = 0").contains("at least 1"));
    assert_eq!(
        ok("version = 1\n[agents]\nmemory = \"512m\"").agent_memory,
        512 << 20
    );
}

#[test]
fn projects_need_absolute_paths_and_merge_into_a_view() {
    assert!(err("version = 1\n[projects.\"relative/p\"]").contains("absolute"));
    assert!(err("version = 1\n[projects.\"~/a/../b\"]").contains(".."));
    let policy = ok(r#"
version = 1
[network.project]
allow = ["api.anthropic.com"]
[builds]
memory = "8g"
[projects."~/p"]
network.project.allow = ["extra.example.org", "api.anthropic.com"]
builds.memory = "16g"
"#);
    let view = policy.for_project(Path::new("/Users/me/p"));
    assert_eq!(
        view.allow
            .iter()
            .map(|d| d.host.as_str())
            .collect::<Vec<_>>(),
        ["api.anthropic.com", "extra.example.org"]
    );
    assert_eq!(view.build_memory, 16 << 30);
    assert_eq!(
        policy
            .for_project(Path::new("/Users/me/other"))
            .build_memory,
        8 << 30
    );
}

#[test]
fn agents_have_their_own_destinations_and_project_approvals() {
    let policy = ok(r#"version = 1
[network.project]
allow = ["registry.npmjs.org"]
[agents]
memory = "2g"
[agents.claude]
allow = ["api.anthropic.com"]
[agents.pi]
allow = ["chatgpt.com"]
local = ["host.containers.internal:11434", "192.168.1.50:1234"]
[projects."/work/app"]
agents.pi.allow = ["example.org"]
"#);
    let app = Path::new("/work/app");
    let pi = policy.for_agent(app, "pi");
    assert_eq!(
        pi.allow,
        [
            Destination {
                host: "chatgpt.com".into(),
                port: 443
            },
            Destination {
                host: "example.org".into(),
                port: 443
            }
        ]
    );
    assert_eq!(
        pi.local[0],
        Destination {
            host: "192.168.1.50".into(),
            port: 1234
        }
    );
    assert_eq!(
        policy.for_agent(app, "claude").allow,
        [Destination {
            host: "api.anthropic.com".into(),
            port: 443
        }]
    );
    assert!(
        policy.for_agent(app, "codex").allow.is_empty(),
        "nothing shared between agents"
    );
    assert_eq!(policy.agent_memory, 2 << 30);
}

#[test]
fn rejects_unknown_agents_and_vague_local_destinations() {
    assert!(err("version = 1\n[agents.claud]\nallow = []").contains("unknown agent"));
    assert!(
        err("version = 1\n[agents.pi]\nlocal = [\"host.containers.internal\"]")
            .contains("needs a port")
    );
    assert!(
        err("version = 1\n[agents.pi]\nlocal = [\"192.168.0.0/16:80\"]")
            .contains("IPv4 address or a lowercase host name")
    );
    assert!(err("version = 1\n[agents.pi]\nextra = 1").contains("unknown field"));
}

#[test]
fn cpu_limits_default_and_validate() {
    let policy = ok("version = 1");
    assert_eq!((policy.agent_cpus, policy.build_cpus), (2.0, 8.0));
    let policy = ok("version = 1\n[agents]\ncpus = 1\n[builds]\ncpus = 6.5\n");
    assert_eq!((policy.agent_cpus, policy.build_cpus), (1.0, 6.5));
    assert!(err("version = 1\n[agents]\ncpus = 0\n").contains("agents.cpus"));
}

#[test]
fn projects_are_identified_by_their_real_path() {
    let base = std::env::temp_dir().join(format!("policy-alias-{}", std::process::id()));
    std::fs::create_dir_all(base.join("real/app")).unwrap();
    let _ = std::fs::remove_file(base.join("alias"));
    std::os::unix::fs::symlink(base.join("real"), base.join("alias")).unwrap();
    let real = std::fs::canonicalize(base.join("real/app")).unwrap();
    let alias = base.join("alias/app");
    let policy = ok(&format!(
        "version = 1\n[projects.\"{}\"]\nagents.pi.allow = [\"example.org\"]\n",
        alias.display()
    ));
    assert_eq!(
        policy.for_agent(&real, "pi").allow.len(),
        1,
        "a table under a symlinked path applies to the real checkout"
    );
    let twice = format!(
        "version = 1\n[projects.\"{}\"]\nengram = \"/x\"\n[projects.\"{}\"]\nengram = \"/y\"\n",
        alias.display(),
        real.display()
    );
    assert!(
        err(&twice).contains("appears twice"),
        "two spellings of one project"
    );
    std::fs::remove_dir_all(base).unwrap();
}
