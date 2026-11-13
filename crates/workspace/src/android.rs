//! The checkout's `local.properties` names the host's Android SDK in `sdk.dir`, which wins over
//! `ANDROID_HOME`. The build container mounts the project's SDK volume at that path too
//! The file is in the checkout, so agents can write it: the path
//! is accepted only under the user's home folder and apart from the checkout, where nothing of
//! the container's own system can be.

use crate::{WorkspaceError, error};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Keys `local.properties` may hold. It's hidden from containers, but a mask over a single file
/// is bypassed through another spelling on a case-insensitive checkout: it must hold nothing
/// secret.
const LOCAL_KEYS: [&str; 2] = ["sdk.dir", "ndk.dir"];

/// Whether the checkout has a `local.properties` to hide; refuses one that holds more than the
/// SDK paths, or isn't a regular file.
pub fn check_local_properties(checkout: &Path) -> Result<bool, WorkspaceError> {
    let file = checkout.join("local.properties");
    let Ok(meta) = fs::symlink_metadata(&file) else {
        return Ok(false);
    };
    if !meta.is_file() {
        return Err(error(
            "local.properties isn't a regular file; make it one or remove it",
        ));
    }
    let text = fs::read_to_string(&file).map_err(|e| error(format!("local.properties: {e}")))?;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
    {
        let key = line.split(['=', ':']).next().unwrap_or("").trim();
        if !LOCAL_KEYS.contains(&key) {
            return Err(error(format!(
                "local.properties holds {key}: it's hidden from agents, but on this case-insensitive checkout another spelling still reads it; move secrets out (only {} may stay)",
                LOCAL_KEYS.join(" and ")
            )));
        }
    }
    Ok(true)
}

/// `sdk.dir` from the checkout's `local.properties`, checked; `None` without one.
pub fn sdk_dir(checkout: &Path, home: &Path) -> Result<Option<PathBuf>, WorkspaceError> {
    let file = checkout.join("local.properties");
    let Ok(meta) = fs::symlink_metadata(&file) else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Err(error(
            "local.properties isn't a regular file; agent-run reads sdk.dir from it",
        ));
    }
    let text = fs::read_to_string(&file).map_err(|e| error(format!("local.properties: {e}")))?;
    let Some(value) = text.lines().rev().find_map(|line| {
        let line = line.trim_start();
        let (key, value) = line.split_once(['=', ':'])?;
        (key.trim() == "sdk.dir").then(|| value.trim().replace("\\:", ":").replace("\\\\", "\\"))
    }) else {
        return Ok(None);
    };
    let path = PathBuf::from(&value);
    let refuse = |why: &str| {
        Err(error(format!(
            "local.properties: sdk.dir = {value:?} {why}"
        )))
    };
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return refuse("must be an absolute path without . or ..");
    }
    if value.contains([',', ':', '\t', '\n']) {
        return refuse("contains a character Podman's mount options can't carry");
    }
    if !path.starts_with(home) || path == home {
        return refuse(&format!(
            "must be inside your home folder ({})",
            home.display()
        ));
    }
    if path.starts_with(checkout) || checkout.starts_with(&path) {
        return refuse("must be apart from the checkout");
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(content: Option<&str>) -> Result<Option<PathBuf>, WorkspaceError> {
        let root = std::env::temp_dir().join(format!(
            "sdk-dir-{}-{}",
            std::process::id(),
            content.map(|c| c.len()).unwrap_or(0)
        ));
        let checkout = root.join("Projects/app");
        fs::create_dir_all(&checkout).unwrap();
        if let Some(content) = content {
            fs::write(
                checkout.join("local.properties"),
                content.replace("HOME", &root.display().to_string()),
            )
            .unwrap();
        }
        let result = sdk_dir(&checkout, &root);
        fs::remove_dir_all(&root).unwrap();
        result.map(|p| {
            p.map(|p| {
                PathBuf::from(
                    p.display()
                        .to_string()
                        .replace(&root.display().to_string(), "HOME"),
                )
            })
        })
    }

    #[test]
    fn local_properties_may_hold_only_the_sdk_paths() {
        let root = std::env::temp_dir().join(format!("local-properties-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        assert!(!check_local_properties(&root).unwrap());
        fs::write(
            root.join("local.properties"),
            "# generated\nsdk.dir=/Users/me/sdk\nndk.dir=/x\n",
        )
        .unwrap();
        assert!(check_local_properties(&root).unwrap());
        fs::write(
            root.join("local.properties"),
            "sdk.dir=/x\nsigning.password=secret\n",
        )
        .unwrap();
        assert!(
            check_local_properties(&root)
                .unwrap_err()
                .0
                .contains("signing.password")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reads_the_host_sdk_path() {
        assert_eq!(check(None).unwrap(), None);
        assert_eq!(check(Some("org.gradle.x=1\n")).unwrap(), None);
        assert_eq!(
            check(Some("## generated\nsdk.dir=HOME/Library/Android/sdk\n")).unwrap(),
            Some(PathBuf::from("HOME/Library/Android/sdk"))
        );
    }

    #[test]
    fn refuses_paths_that_could_cover_the_container_or_the_checkout() {
        for (content, why) in [
            ("sdk.dir=/usr/local/bin\n", "inside your home folder"),
            ("sdk.dir=HOME\n", "inside your home folder"),
            ("sdk.dir=relative/sdk\n", "absolute path"),
            ("sdk.dir=HOME/x/../../usr\n", "absolute path"),
            ("sdk.dir=HOME/Projects/app/sdk\n", "apart from the checkout"),
            ("sdk.dir=HOME/Projects\n", "apart from the checkout"),
            ("sdk.dir=HOME/a,b\n", "character"),
        ] {
            let e = check(Some(content)).expect_err(content).0;
            assert!(e.contains(why), "{content}: {e}");
        }
    }
}
