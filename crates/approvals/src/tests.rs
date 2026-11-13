use super::*;

const HOME: &str = "/Users/me";

fn dest(text: &str) -> Destination {
    destination(text).unwrap()
}

fn project(checkout: &str) -> List<'static> {
    List::Agent {
        checkout: Box::leak(PathBuf::from(checkout).into_boxed_path()),
        home: Path::new(HOME),
        agent: "pi",
    }
}

const POLICY: &str = r#"version = 1

[network.project]      # every project
allow = ["api.anthropic.com"]   # the agent's API

[workspace]
mask = [".signing"]

# One table per project.
[projects."~/Projects/tmc"]
agents.pi.allow = ["example.org"]   # an approved destination
engram = "~/engram/tmc"
"#;

fn valid(text: &str) -> policy::Policy {
    policy::parse(text, Path::new(HOME)).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

#[test]
fn adds_to_an_existing_project_table_keeping_comments() {
    let text = policy_add(
        POLICY,
        project("/Users/me/Projects/tmc"),
        &dest("maven.example.com:8443"),
    )
    .unwrap()
    .unwrap();
    assert!(text.contains("agents.pi.allow = [\"example.org\", \"maven.example.com:8443\"]   # an approved destination"), "{text}");
    assert!(
        text.contains("# One table per project.")
            && text.contains("allow = [\"api.anthropic.com\"]   # the agent's API")
    );
    let policy = valid(&text);
    assert_eq!(
        policy
            .for_agent(Path::new("/Users/me/Projects/tmc"), "pi")
            .allow
            .len(),
        2
    );
    assert!(
        policy
            .for_agent(Path::new("/Users/me/Projects/tmc"), "claude")
            .allow
            .is_empty(),
        "never another agent's"
    );
}

#[test]
fn creates_a_project_table_with_dotted_keys() {
    let text = policy_add(
        POLICY,
        project("/Users/me/Projects/tfa"),
        &dest("example.net"),
    )
    .unwrap()
    .unwrap();
    assert!(
        text.ends_with("[projects.\"~/Projects/tfa\"]\nagents.pi.allow = [\"example.net\"]\n"),
        "{text}"
    );
    assert_eq!(
        valid(&text)
            .for_agent(Path::new("/Users/me/Projects/tfa"), "pi")
            .allow,
        vec![dest("example.net")]
    );
    let outside = policy_add("version = 1\n", project("/opt/work"), &dest("example.net"))
        .unwrap()
        .unwrap();
    assert!(
        outside.contains("[projects.\"/opt/work\"]\nagents.pi.allow = [\"example.net\"]"),
        "{outside}"
    );
    valid(&outside);
}

#[test]
fn adds_and_removes_globally() {
    let text = policy_add(POLICY, List::Global, &dest("registry.npmjs.org"))
        .unwrap()
        .unwrap();
    assert_eq!(
        valid(&text).project_allow,
        vec![dest("api.anthropic.com"), dest("registry.npmjs.org")]
    );
    let text = policy_remove(&text, List::Global, &dest("api.anthropic.com:443"))
        .unwrap()
        .unwrap();
    assert_eq!(valid(&text).project_allow, vec![dest("registry.npmjs.org")]);
    let created = policy_add("version = 1\n", List::Global, &dest("example.net"))
        .unwrap()
        .unwrap();
    assert_eq!(
        created,
        "version = 1\n\n[network.project]\nallow = [\"example.net\"]\n"
    );
}

#[test]
fn matches_destinations_not_spellings() {
    assert_eq!(
        policy_add(
            POLICY,
            project("/Users/me/Projects/tmc"),
            &dest("example.org:443")
        )
        .unwrap(),
        None
    );
    assert!(
        policy_contains(
            POLICY,
            project("/Users/me/Projects/tmc"),
            &dest("example.org")
        )
        .unwrap()
    );
    assert!(
        !policy_contains(
            POLICY,
            project("/Users/me/Projects/tmc"),
            &dest("example.org:8443")
        )
        .unwrap()
    );
    assert!(
        !policy_contains(
            POLICY,
            project("/Users/me/Projects/tfa"),
            &dest("example.org")
        )
        .unwrap()
    );
    let text = policy_remove(
        POLICY,
        project("/Users/me/Projects/tmc"),
        &dest("example.org"),
    )
    .unwrap()
    .unwrap();
    assert!(text.contains("agents.pi.allow = []"), "{text}");
    assert_eq!(
        policy_remove(
            POLICY,
            project("/Users/me/Projects/tfa"),
            &dest("example.org")
        )
        .unwrap(),
        None
    );
}

#[test]
fn refuses_layouts_it_would_have_to_guess() {
    for text in [
        "version = 1\nnetwork = 3\n",
        "version = 1\n[network]\nproject = { allow = \"x\" }\n",
    ] {
        assert!(
            policy_add(text, List::Global, &dest("example.net"))
                .unwrap_err()
                .0
                .contains("edit the policy by hand"),
            "{text}"
        );
    }
}

#[test]
fn session_approvals_are_a_file_per_project() {
    let state = std::env::temp_dir().join(format!("approvals-test-{}", std::process::id()));
    let session = Session::new(&state, "tmc-1234", "pi");
    assert!(session.list().unwrap().is_empty());
    assert!(session.add(&dest("example.org")).unwrap());
    assert!(!session.add(&dest("example.org:443")).unwrap());
    assert!(session.add(&dest("example.net:8443")).unwrap());
    assert_eq!(
        fs::read_to_string(state.join("approvals/tmc-1234/pi")).unwrap(),
        "example.org\nexample.net:8443\n"
    );
    assert!(
        Session::new(&state, "tmc-1234", "claude")
            .list()
            .unwrap()
            .is_empty(),
        "never another agent's"
    );
    assert!(session.remove(&dest("example.org")).unwrap());
    assert!(!session.remove(&dest("example.org")).unwrap());
    assert_eq!(session.list().unwrap(), vec![dest("example.net:8443")]);
    Session::clear_project(&state, "tmc-1234").unwrap();
    assert!(session.list().unwrap().is_empty());
    fs::remove_dir_all(state).unwrap();
}

#[test]
fn edits_are_checked_before_they_are_written() {
    let dir = std::env::temp_dir().join(format!("approvals-edit-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("policy.toml");
    fs::write(&path, POLICY).unwrap();
    let broken = edit_policy(&path, Path::new(HOME), |_| Ok(Some("version = 2\n".into())));
    assert!(broken.unwrap_err().0.contains("wasn't written"));
    assert_eq!(fs::read_to_string(&path).unwrap(), POLICY);
    assert!(
        edit_policy(&path, Path::new(HOME), |t| policy_add(
            t,
            List::Global,
            &dest("example.net")
        ))
        .unwrap()
    );
    assert!(
        !edit_policy(&path, Path::new(HOME), |t| policy_add(
            t,
            List::Global,
            &dest("example.net")
        ))
        .unwrap()
    );
    fs::remove_dir_all(dir).unwrap();
}
