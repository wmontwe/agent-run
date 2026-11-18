use super::*;
use std::path::Path;

#[test]
fn build_identity_is_reused_per_project_and_removed_with_its_pod() {
    let unique = egress::Identity::new("test").unwrap();
    let base = std::env::temp_dir().join(format!("agent-run-{}", unique.user));
    let paths = Paths::new(&base, "pi");
    let project = Project::new(&paths.namespace, Path::new("/work/app"));
    let other = Project::new(&paths.namespace, Path::new("/work/other"));
    let first = build_identity_for_generation(&paths, &project, "generation-a").unwrap();
    let reused = build_identity_for_generation(&paths, &project, "generation-a").unwrap();
    let separate = build_identity_for_generation(&paths, &other, "generation-a").unwrap();
    let next_pod = build_identity_for_generation(&paths, &project, "generation-b").unwrap();
    assert_ne!(
        first.user, next_pod.user,
        "a replacement pod must not inherit old credentials"
    );
    assert_eq!(first.user, reused.user);
    assert_ne!(first.user, separate.user);
    assert!(
        !paths.agent_home.exists(),
        "credentials must stay outside agent mounts"
    );
    Services::forget_build(&paths, &project);
    assert!(!build_credentials(&paths, &project, "generation-a").exists());
    assert!(build_credentials(&paths, &other, "generation-a").exists());
    let replacement = build_identity_for_generation(&paths, &project, "generation-c").unwrap();
    assert_ne!(first.user, replacement.user);
    Services::forget_all(&paths);
    assert!(!paths.state.join("build-proxies").exists());
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn an_unreadable_build_identity_only_costs_its_own_pod() {
    let unique = egress::Identity::new("test").unwrap();
    let base = std::env::temp_dir().join(format!("agent-run-{}", unique.user));
    let paths = Paths::new(&base, "pi");
    let project = Project::new(&paths.namespace, Path::new("/work/app"));
    let good = build_identity_for_generation(&paths, &project, "generation-a").unwrap();
    let path = build_credentials(&paths, &project, "generation-a");
    assert_eq!(rules_build_identity(&path).unwrap().user, good.user);
    // An identity from an older agent-run (an MD5-crypt line): the rules go on without it.
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n{}:$1$abcdefgh$G//4keteveJp0qb8z2DxG/\n",
            good.user, good.url, good.user
        ),
    )
    .unwrap();
    assert!(egress::Identity::load(&path, "build").is_err());
    assert!(rules_build_identity(&path).is_none());
    std::fs::remove_dir_all(base).unwrap();
}
