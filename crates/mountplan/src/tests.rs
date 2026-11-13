use super::*;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU32, Ordering};
use workspace::Rules;

fn scratch() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mountplan-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

/// A checkout with a root build, an included build, nested module state and rule folders.
fn checkout() -> (PathBuf, Inspection) {
    let root = scratch().join("checkout");
    for dir in [
        ".git",
        ".signing",
        "gradle/wrapper",
        "app/build",
        "lib/nested/build",
        "build-logic/.kotlin",
        "kotlin-js-store",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("settings.gradle.kts"), "").unwrap();
    fs::write(root.join("build-logic/settings.gradle.kts"), "").unwrap();
    let (mask, read_only, private) = (
        strings(&[".signing"]),
        strings(&[".git", "gradle/wrapper"]),
        strings(&["kotlin-js-store"]),
    );
    let inspection = workspace::inspect(
        &root,
        &Rules {
            mask: &mask,
            read_only: &read_only,
            private: &private,
        },
    )
    .unwrap();
    (root, inspection)
}

fn kinds(plan: &Plan) -> Vec<(String, Kind)> {
    plan.mounts
        .iter()
        .map(|m| (m.relative.clone(), m.kind.clone()))
        .collect()
}

fn private(relative: &str) -> Kind {
    Kind::Private {
        subpath: format!("private/{relative}"),
    }
}

#[test]
fn plans_parents_before_children_with_pins_for_every_nested_target() {
    let (root, inspection) = checkout();
    let plan = plan(&root, &inspection, "state").unwrap();
    assert_eq!(
        kinds(&plan),
        [
            ("".into(), Kind::Workspace),
            (".git".into(), Kind::ReadOnly),
            (".signing".into(), Kind::Mask),
            ("app".into(), Kind::Pin),
            ("build".into(), private("build")),
            ("build-logic".into(), Kind::Pin),
            ("gradle".into(), Kind::Pin),
            ("kotlin-js-store".into(), private("kotlin-js-store")),
            ("lib".into(), Kind::Pin),
            ("app/build".into(), Kind::Mask),
            ("build-logic/.kotlin".into(), Kind::Mask),
            ("build-logic/build".into(), private("build-logic/build")),
            ("gradle/wrapper".into(), Kind::ReadOnly),
            ("lib/nested".into(), Kind::Pin),
            ("lib/nested/build".into(), Kind::Mask),
        ]
    );
    let inode = |relative: &str| {
        plan.mounts
            .iter()
            .find(|m| m.relative == relative)
            .unwrap()
            .host_ino
    };
    assert_eq!(
        inode("gradle/wrapper"),
        Some(fs::metadata(root.join("gradle/wrapper")).unwrap().ino())
    );
    assert!(inode("lib").is_some() && inode("app/build").is_none());
    assert_eq!(
        plan.subpaths(),
        [
            "private/build",
            "private/kotlin-js-store",
            "private/build-logic/build"
        ]
    );
}

#[test]
fn turns_the_plan_into_podman_arguments() {
    let (root, inspection) = checkout();
    let plan = plan(&root, &inspection, "state").unwrap();
    let args = plan.podman_args().join(" ");
    let r = root.display();
    assert!(args.starts_with(&format!("--mount type=bind,src={r},dst={r} ")));
    assert!(args.contains(&format!(
        "--mount type=bind,src={r}/gradle/wrapper,dst={r}/gradle/wrapper,ro"
    )));
    assert!(args.contains(&format!(
        "--mount type=bind,src={r}/lib/nested,dst={r}/lib/nested "
    )));
    assert!(args.contains(&format!(
        "--mount type=volume,src=state,dst={r}/build,subpath=private/build"
    )));
    assert!(args.ends_with(&format!("--security-opt mask={r}/.signing:{r}/app/build:{r}/build-logic/.kotlin:{r}/lib/nested/build")));
}

#[test]
fn refuses_symlinks_and_unsafe_characters() {
    let (root, mut inspection) = checkout();
    let outside = scratch();
    fs::create_dir_all(outside.join("wrapper")).unwrap();
    fs::rename(root.join("gradle"), root.join("gradle-real")).unwrap();
    symlink(&outside, root.join("gradle")).unwrap();
    assert!(
        plan(&root, &inspection, "state")
            .unwrap_err()
            .0
            .contains("symlink")
    );

    let (root, _) = checkout();
    fs::create_dir_all(root.join("a,b")).unwrap();
    inspection.read_only = strings(&["a,b"]);
    inspection.mask.clear();
    assert!(
        plan(&root, &inspection, "state")
            .unwrap_err()
            .0
            .contains("','")
    );
}

#[test]
fn round_trips_its_text_form_and_hashes_it() {
    let (root, inspection) = checkout();
    let plan = plan(&root, &inspection, "state").unwrap();
    assert_eq!(Plan::from_text(&plan.to_text()).unwrap(), plan);
    let mut changed = plan.clone();
    changed.mounts.pop();
    assert_ne!(changed.hash(), plan.hash());
    assert_eq!(plan.hash().len(), 32);
    assert!(Plan::from_text("checkout\t/x\nvolume\tv\nbogus\ta\t-\t\n").is_err());
    assert_ne!(
        rules_hash(&strings(&["a"]), &[], &[]),
        rules_hash(&[], &strings(&["a"]), &[])
    );
}

/// The report a container matching `plan` would give, as the gate prints it.
fn report_for(plan: &Plan) -> String {
    let mut text = String::new();
    let share_root = |relative: &str| {
        format!("/host{}", plan.checkout.join(relative).display())
            .trim_end_matches('/')
            .to_owned()
    };
    for (index, mount) in plan.mounts.iter().enumerate() {
        let target = plan.target(mount).display().to_string();
        let line = match &mount.kind {
            Kind::Workspace | Kind::Pin => format!(
                "{index} 1 0:50 {} {target} rw,relatime - virtiofs share rw",
                share_root(&mount.relative)
            ),
            Kind::ReadOnly => format!(
                "{index} 1 0:50 {} {target} ro,relatime - virtiofs share rw",
                share_root(&mount.relative)
            ),
            Kind::Mask => format!("{index} 1 0:60 / {target} ro,relatime - tmpfs tmpfs ro"),
            Kind::Hide => format!("{index} 1 0:7 /null {target} ro,nosuid - devtmpfs devtmpfs rw"),
            Kind::Private { subpath } => format!(
                "{index} 1 0:70 /containers/storage/volumes/state/_data/{subpath} {target} rw - xfs /dev/vda4 rw"
            ),
        };
        text.push_str(&format!("mount {line}\n"));
    }
    for (index, mount) in plan.mounts.iter().enumerate() {
        text.push_str(&format!(
            "target {index} 50 {}\n",
            mount.host_ino.unwrap_or(1000 + index as u64)
        ));
    }
    text
}

fn problems(plan: &Plan, text: &str) -> Vec<String> {
    verify(plan, &parse_report(text, plan.mounts.len()).unwrap())
}

#[test]
fn accepts_a_container_that_matches_the_plan() {
    let (root, inspection) = checkout();
    let plan = plan(&root, &inspection, "state").unwrap();
    assert_eq!(problems(&plan, &report_for(&plan)), Vec::<String>::new());
}

#[test]
fn blocks_every_tampering_case() {
    let (root, inspection) = checkout();
    let plan = plan(&root, &inspection, "state").unwrap();
    let good = report_for(&plan);
    let r = root.display().to_string();
    let wrapper = plan
        .mounts
        .iter()
        .position(|m| m.relative == "gradle/wrapper")
        .unwrap();
    let wrapper_ino = plan.mounts[wrapper].host_ino.unwrap();

    let cases: Vec<(&str, String, &str)> = vec![
        (
            "rearranged folder",
            good.replace(
                &format!("target {wrapper} 50 {wrapper_ino}"),
                &format!("target {wrapper} 50 999999"),
            ),
            "inode differs",
        ),
        (
            "missing mask",
            good.lines()
                .filter(|l| !l.ends_with(&format!("{r}/.signing ro,relatime - tmpfs tmpfs ro")))
                .collect::<Vec<_>>()
                .join("\n"),
            ".signing not mounted",
        ),
        (
            "swapped subpath",
            good.replace("_data/private/build ", "_data/private/kotlin-js-store "),
            "build: source",
        ),
        (
            "unplanned mount",
            format!("{good}mount 99 1 0:50 /host{r}/app {r}/app/src ro - virtiofs share rw\n"),
            "unplanned mount app/src",
        ),
        (
            "read-only made read-write",
            good.replace(
                &format!("{r}/gradle/wrapper ro,relatime"),
                &format!("{r}/gradle/wrapper rw,relatime"),
            ),
            "mounted read-write",
        ),
        (
            "mask on a writable folder",
            good.replace(
                &format!("{r}/app/build ro,relatime - tmpfs"),
                &format!("{r}/app/build rw,relatime - tmpfs"),
            ),
            "not masked",
        ),
        (
            "pin from elsewhere",
            good.replace(
                &format!("/host{r}/lib {r}/lib "),
                &format!("/host/tmp/lib {r}/lib "),
            ),
            "lib: source",
        ),
    ];
    for (name, report, expected) in cases {
        let found = problems(&plan, &report);
        assert!(
            found.iter().any(|p| p.contains(expected)),
            "{name}: expected {expected:?}, got {found:?}"
        );
    }
    assert_eq!(problems(&plan, "target 0 50 1\n"), ["checkout not mounted"]);
}

#[test]
fn local_properties_is_hidden_and_verified_as_dev_null() {
    let (root, _) = checkout();
    fs::write(root.join("local.properties"), "sdk.dir=/Users/me/sdk\n").unwrap();
    let (mask, read_only, private) = (strings(&[".signing"]), strings(&[".git"]), strings(&[]));
    let inspection = workspace::inspect(
        &root,
        &Rules {
            mask: &mask,
            read_only: &read_only,
            private: &private,
        },
    )
    .unwrap();
    let plan = plan(&root, &inspection, "state").unwrap();
    assert!(kinds(&plan).contains(&("local.properties".into(), Kind::Hide)));
    assert!(
        plan.podman_args()
            .join(" ")
            .contains(&format!("{}/local.properties", root.display()))
    );
    assert_eq!(Plan::from_text(&plan.to_text()).unwrap(), plan);
    let report = report_for(&plan);
    assert!(
        problems(&plan, &report).is_empty(),
        "{:?}",
        problems(&plan, &report)
    );
    let shown = report.replace(
        &format!("/null {}/local.properties", root.display()),
        &format!(
            "/host{0}/local.properties {0}/local.properties",
            root.display()
        ),
    );
    assert!(
        problems(&plan, &shown)
            .iter()
            .any(|p| p.contains("not hidden")),
        "an unhidden file is caught"
    );
}
