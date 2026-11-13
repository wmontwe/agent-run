//! Where a checkout may be: it's mounted at its own path in the
//! containers, so it must not cover their system folders.

use crate::{WorkspaceError, error};
use std::path::Path;

/// Folders of the containers' own systems.
const SYSTEM: [&str; 17] = [
    "/bin",
    "/boot",
    "/build-state",
    "/dev",
    "/etc",
    "/home",
    "/knowledge",
    "/lib",
    "/opt",
    "/proc",
    "/root",
    "/run",
    "/sbin",
    "/srv",
    "/sys",
    "/tmp",
    "/usr",
];

/// Refuses a checkout under a container system folder.
pub fn check_location(checkout: &Path) -> Result<(), WorkspaceError> {
    if checkout == Path::new("/")
        || SYSTEM.iter().any(|s| checkout.starts_with(s))
        || checkout.starts_with("/var")
    {
        return Err(error(format!(
            "{} is where the containers' own system is; use a checkout elsewhere",
            checkout.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_system_folders() {
        assert!(check_location(Path::new("/Users/me/Projects/app")).is_ok());
        assert!(
            check_location(Path::new("/private/var/folders/x/app")).is_ok(),
            "macOS temp folders are fine"
        );
        for checkout in ["/opt/app", "/usr/local/src/app", "/home/node", "/"] {
            assert!(
                check_location(Path::new(checkout))
                    .unwrap_err()
                    .0
                    .contains("own system"),
                "{checkout}"
            );
        }
    }
}
