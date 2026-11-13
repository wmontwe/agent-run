//! The Gradle wrapper's distribution, installed from the mirror.
//! The wrapper's URL can't be changed (`gradle/wrapper/` is read-only, and the wrapper reads
//! it before any init script), so the distribution is put where the wrapper looks first:
//! `<Gradle home>/wrapper/dists/<name>/<base-36 MD5 of the URL>/`, with its `.ok` marker.

use md5::{Digest as _, Md5};
use sha2::Sha256;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const OFFICIAL: &str = "https://services.gradle.org/distributions/";

/// The parts of `gradle-wrapper.properties` the install needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapper {
    pub url: String,
    pub sha256: Option<String>,
    /// Whether the distribution goes to the Gradle home's `wrapper/dists`, the default.
    pub default_location: bool,
}

/// Java properties: `key=value` or `key: value`, `#` and `!` comments, `\` escapes.
pub fn properties(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let mut key = String::new();
        let mut chars = line.chars();
        let mut escaped = false;
        for c in chars.by_ref() {
            if escaped {
                key.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '=' || c == ':' {
                break;
            } else {
                key.push(c);
            }
        }
        let mut value = String::new();
        let mut escaped = false;
        for c in chars.as_str().trim_start().chars() {
            if escaped {
                value.push(match c {
                    'n' => '\n',
                    't' => '\t',
                    other => other,
                });
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else {
                value.push(c);
            }
        }
        out.push((key.trim_end().to_owned(), value));
    }
    out
}

pub fn read_wrapper(root: &Path) -> Result<Option<Wrapper>, String> {
    let file = root.join("gradle/wrapper/gradle-wrapper.properties");
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let props = properties(&text);
    let get = |key: &str| {
        props
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    let url =
        get("distributionUrl").ok_or_else(|| format!("{}: no distributionUrl", file.display()))?;
    let default_location = get("distributionBase").is_none_or(|b| b == "GRADLE_USER_HOME")
        && get("distributionPath").is_none_or(|p| p == "wrapper/dists");
    Ok(Some(Wrapper {
        url,
        sha256: get("distributionSha256Sum").map(|s| s.to_lowercase()),
        default_location,
    }))
}

/// The folder the wrapper unpacks `url` into, relative to the Gradle home.
pub fn dist_dir(url: &str) -> Result<PathBuf, String> {
    let file = url
        .rsplit('/')
        .next()
        .filter(|f| f.ends_with(".zip"))
        .ok_or_else(|| format!("{url}: not a .zip distribution"))?;
    let name = &file[..file.len() - 4];
    Ok(Path::new("wrapper/dists")
        .join(name)
        .join(base36(u128::from_be_bytes(
            Md5::digest(url.as_bytes()).into(),
        ))))
}

/// Like Java's `new BigInteger(1, bytes).toString(36)`.
fn base36(mut n: u128) -> String {
    if n == 0 {
        return "0".to_owned();
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push(b"0123456789abcdefghijklmnopqrstuvwxyz"[(n % 36) as usize]);
        n /= 36;
    }
    digits.reverse();
    String::from_utf8(digits).unwrap()
}

/// Makes sure the wrapper finds its distribution in `gradle_home`, downloading it from the
/// mirror's `gradle-distributions` repository if needed and checking its SHA-256 (the
/// published one, and the wrapper's own if it names one). Returns a message when it did.
/// Stops when `cancelled`; what it left is redone by the next install.
pub fn install(
    root: &Path,
    gradle_home: &Path,
    mirror: &str,
    cancelled: &AtomicBool,
) -> Result<Option<String>, String> {
    let Some(wrapper) = read_wrapper(root)? else {
        return Ok(None);
    };
    if !wrapper.default_location {
        return Ok(None);
    }
    let file = wrapper
        .url
        .strip_prefix(OFFICIAL)
        .filter(|f| !f.contains('/'))
        .ok_or_else(|| format!("the wrapper's distributionUrl {} isn't on {OFFICIAL}; only those come through the mirror", wrapper.url))?;
    let dir = gradle_home.join(dist_dir(&wrapper.url)?);
    let marker = dir.join(format!("{file}.ok"));
    if marker.exists() {
        return Ok(None);
    }
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let source = format!(
        "{}/gradle-distributions/{file}",
        mirror.trim_end_matches('/')
    );
    let zip = dir.join(file);
    let part = dir.join(format!("{file}.part"));
    curl(&source, &part, cancelled)?;
    let published = curl_text(&format!("{source}.sha256"), cancelled)?;
    let actual = sha256(&part)?;
    let expected: Vec<&str> = std::iter::once(published.split_whitespace().next().unwrap_or(""))
        .chain(wrapper.sha256.as_deref())
        .collect();
    if let Some(wrong) = expected.iter().find(|e| !e.eq_ignore_ascii_case(&actual)) {
        let _ = fs::remove_file(&part);
        return Err(format!(
            "{file}: SHA-256 {actual}, expected {wrong}; not installed"
        ));
    }
    run(
        Command::new("unzip")
            .args(["-q", "-o"])
            .arg(&part)
            .arg("-d")
            .arg(&dir),
        "unzip",
        cancelled,
    )?;
    fs::rename(&part, &zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    fs::write(&marker, "").map_err(|e| format!("{}: {e}", marker.display()))?;
    Ok(Some(format!("installed {file} from the mirror")))
}

/// Runs a command to its end, or kills it once `cancelled`; returns its standard output.
fn run(command: &mut Command, name: &str, cancelled: &AtomicBool) -> Result<String, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{name}: {e}"))?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| format!("{name}: {e}"))? {
            break status;
        }
        if cancelled.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{name}: cancelled"));
        }
        thread::sleep(Duration::from_millis(50));
    };
    let (stdout, stderr) = (
        stdout.join().unwrap_or_default(),
        stderr.join().unwrap_or_default(),
    );
    if status.success() {
        Ok(String::from_utf8_lossy(&stdout).into_owned())
    } else {
        Err(format!(
            "{name}: {}",
            String::from_utf8_lossy(&stderr).trim()
        ))
    }
}

fn curl(url: &str, to: &Path, cancelled: &AtomicBool) -> Result<(), String> {
    run(
        Command::new("curl")
            .args(["-fsSL", "--noproxy", "*", "-o"])
            .arg(to)
            .arg(url),
        &format!("downloading {url}"),
        cancelled,
    )
    .map(|_| ())
}

fn curl_text(url: &str, cancelled: &AtomicBool) -> Result<String, String> {
    run(
        Command::new("curl").args(["-fsSL", "--noproxy", "*", url]),
        &format!("downloading {url}"),
        cancelled,
    )
}

fn sha256(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_wrapper_properties() {
        let text = "#Generated\ndistributionBase=GRADLE_USER_HOME\ndistributionPath=wrapper/dists\n\
            distributionUrl=https\\://services.gradle.org/distributions/gradle-8.14.3-bin.zip\n\
            distributionSha256Sum=ABC123\nnetworkTimeout=10000\n";
        let dir = std::env::temp_dir().join(format!("wrapper-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("gradle/wrapper")).unwrap();
        fs::write(dir.join("gradle/wrapper/gradle-wrapper.properties"), text).unwrap();
        let wrapper = read_wrapper(&dir).unwrap().unwrap();
        assert_eq!(
            wrapper.url,
            "https://services.gradle.org/distributions/gradle-8.14.3-bin.zip"
        );
        assert_eq!(wrapper.sha256.as_deref(), Some("abc123"));
        assert!(wrapper.default_location);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_the_folder_like_the_wrapper() {
        // Folders the wrapper itself created for these URLs.
        for (url, dir) in [
            (
                "https://services.gradle.org/distributions/gradle-9.7.0-all.zip",
                "wrapper/dists/gradle-9.7.0-all/5hez1j29szzu41ldlyeeyuiv4",
            ),
            (
                "https://services.gradle.org/distributions/gradle-9.7.0-bin.zip",
                "wrapper/dists/gradle-9.7.0-bin/d4tj7w02tcgubx9zk9hbippn6",
            ),
            (
                "https://services.gradle.org/distributions/gradle-8.5-all.zip",
                "wrapper/dists/gradle-8.5-all/3zlzzgtsutfj0pbojr50n2l7z",
            ),
        ] {
            assert_eq!(dist_dir(url).unwrap(), Path::new(dir), "{url}");
        }
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }

    #[test]
    fn parses_properties_like_java() {
        let props = properties("a = 1\nb:2\n  # comment\n!also\nc\\:d=e\\\\f\n");
        assert_eq!(
            props,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c:d".into(), "e\\f".into())
            ]
        );
    }
}
