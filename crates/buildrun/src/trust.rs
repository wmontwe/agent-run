//! Trust for the mirror's HTTPS in the build container. The container root is read-only, so the
//! server writes, before it serves anything, a Java trust store (the JDK's authorities plus
//! agent-run's) and a PEM bundle (the system's plus agent-run's) for curl and OpenSSL. The
//! container's environment points at them (see [`java_options`]).

use std::fs;
use std::path::Path;
use std::process::Command;

/// agent-run's authority's certificate, mounted read-only.
pub const CA: &str = "/etc/agent-run/mirror-ca.crt";
/// What the server writes, in the container's `/tmp`.
pub const DIR: &str = "/tmp/agent-run";
pub const TRUSTSTORE: &str = "/tmp/agent-run/truststore.p12";
pub const BUNDLE: &str = "/tmp/agent-run/ca-bundle.pem";
/// The system's authorities, from the image's ca-certificates.
const SYSTEM_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// The JDK's own; the trust store holds no secrets.
const PASSWORD: &str = "changeit";

/// JVM options that make every JVM trust the mirror, and every public authority as before.
pub fn java_options() -> String {
    format!("-Djavax.net.ssl.trustStore={TRUSTSTORE} -Djavax.net.ssl.trustStorePassword={PASSWORD}")
}

/// Writes the trust store and the bundle with `jdk`'s `keytool` and authorities.
pub fn prepare(jdk: &Path, ca: &Path) -> Result<(), String> {
    let io = |path: &str, e: std::io::Error| format!("{path}: {e}");
    fs::create_dir_all(DIR).map_err(|e| io(DIR, e))?;
    let cacerts = jdk.join("lib/security/cacerts");
    fs::copy(&cacerts, TRUSTSTORE).map_err(|e| format!("{}: {e}", cacerts.display()))?;
    let output = Command::new(jdk.join("bin/keytool"))
        .args([
            "-importcert",
            "-noprompt",
            "-alias",
            "agent-run-mirror",
            "-file",
        ])
        .arg(ca)
        .args(["-keystore", TRUSTSTORE, "-storepass", PASSWORD])
        .output()
        .map_err(|e| format!("keytool: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "keytool couldn't add {} to the trust store: {}",
            ca.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let system = fs::read_to_string(SYSTEM_BUNDLE).map_err(|e| io(SYSTEM_BUNDLE, e))?;
    let ours = fs::read_to_string(ca).map_err(|e| format!("{}: {e}", ca.display()))?;
    fs::write(BUNDLE, bundle(&system, &ours)).map_err(|e| io(BUNDLE, e))
}

/// The system's authorities followed by agent-run's, each certificate on its own lines.
fn bundle(system: &str, ours: &str) -> String {
    let mut text = system.trim_end().to_owned();
    text.push('\n');
    text.push_str(ours.trim_end());
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_keeps_every_public_authority_and_adds_ours() {
        let system = "-----BEGIN CERTIFICATE-----\nA\n-----END CERTIFICATE-----";
        let ours = "-----BEGIN CERTIFICATE-----\nB\n-----END CERTIFICATE-----\n\n";
        assert_eq!(
            bundle(system, ours),
            format!("{system}\n-----BEGIN CERTIFICATE-----\nB\n-----END CERTIFICATE-----\n")
        );
        assert!(java_options().contains(TRUSTSTORE));
    }
}
