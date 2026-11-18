//! Proxy credentials. Persistent build identities live only in trusted host state.

use crate::{ALIAS, PORT, htpasswd_line, htpasswd_matches};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub struct Identity {
    pub user: String,
    pub url: String,
    pub password_line: String,
}

impl Identity {
    pub fn new(role: &str) -> Result<Self, String> {
        if role.is_empty() || !role.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err("proxy identity role must contain lowercase letters only".into());
        }
        let random = |count| -> Result<String, String> {
            let mut bytes = vec![0u8; count];
            getrandom::fill(&mut bytes).map_err(|e| format!("proxy credentials: {e}"))?;
            Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
        };
        let user = format!("{role}-{}", random(6)?);
        let password = random(16)?;
        Ok(Self {
            url: format!("http://{user}:{password}@{ALIAS}:{PORT}"),
            password_line: htpasswd_line(&user, &password)?,
            user,
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let io = |e| format!("{}: {e}", path.display());
        fs::create_dir_all(
            path.parent()
                .ok_or("proxy identity needs a parent folder")?,
        )
        .map_err(io)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(io)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(io)?;
        // In place: the VM must never see a transient missing state file.
        write!(
            file,
            "{}\n{}\n{}\n",
            self.user, self.url, self.password_line
        )
        .map_err(io)
    }

    pub fn load(path: &Path, role: &str) -> Result<Option<Self>, String> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let bad = || {
            format!(
                "{}: invalid proxy identity; restart the project",
                path.display()
            )
        };
        let lines: Vec<_> = text.lines().collect();
        let [user, url, password_line] = lines.as_slice() else {
            return Err(bad());
        };
        let hex = |s: &str, len| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
        if !user
            .strip_prefix(&format!("{role}-"))
            .is_some_and(|id| hex(id, 12))
        {
            return Err(bad());
        }
        let prefix = format!("http://{user}:");
        let suffix = format!("@{ALIAS}:{PORT}");
        let password = url
            .strip_prefix(&prefix)
            .and_then(|s| s.strip_suffix(&suffix))
            .filter(|p| hex(p, 32))
            .ok_or_else(bad)?;
        if !htpasswd_matches(password_line, user, password) {
            return Err(bad());
        }
        Ok(Some(Self {
            user: (*user).into(),
            url: (*url).into(),
            password_line: (*password_line).into(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_credentials_round_trip_privately_and_reject_other_roles() {
        let identity = Identity::new("build").unwrap();
        let dir = std::env::temp_dir().join(format!(
            "proxy-identity-{}-{}",
            std::process::id(),
            identity.user
        ));
        let path = dir.join("identity");
        assert!(Identity::load(&path, "build").unwrap().is_none());
        identity.save(&path).unwrap();
        let loaded = Identity::load(&path, "build").unwrap().unwrap();
        assert_eq!(identity.user, loaded.user);
        assert_eq!(identity.url, loaded.url);
        assert_eq!(identity.password_line, loaded.password_line);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(Identity::load(&path, "pi").is_err());
        fs::write(&path, "bad identity containing a secret").unwrap();
        let message = Identity::load(&path, "build").err().unwrap();
        assert!(!message.contains("containing a secret"));
        fs::remove_dir_all(dir).unwrap();
    }
}
