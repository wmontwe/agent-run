//! The mirror's HTTPS: a certificate authority of agent-run's own, created once in trusted host
//! state, and the mirror's certificate for its name on the pods' networks. Kotlin Toolchain
//! resolves dependencies from HTTPS repositories only. Build containers trust the authority's
//! certificate; its key never leaves the host.

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The authority and the mirror's certificate, in `dir` (host state, never mounted whole).
pub struct Tls {
    dir: PathBuf,
}

/// What the mirror serves with: its certificate and key, PEM.
pub struct ServerCert {
    pub cert: String,
    pub key: String,
}

const CA_KEY: &str = "ca.key";
const CA_CERT: &str = "ca.crt";
const SERVER_KEY: &str = "mirror.key";
const SERVER_CERT: &str = "mirror.crt";
/// Valid for ten years from creation; removing the folder makes new ones.
const YEARS: i32 = 10;

fn error(path: &Path, e: impl std::fmt::Display) -> String {
    format!("{}: {e}", path.display())
}

impl Tls {
    pub fn new(dir: PathBuf) -> Tls {
        Tls { dir }
    }

    /// The authority's certificate, which build containers trust.
    pub fn ca_cert(&self) -> PathBuf {
        self.dir.join(CA_CERT)
    }

    /// Creates the authority and the mirror's certificate if they're missing, and returns the
    /// mirror's. Existing ones are kept: a new certificate would restart the mirror.
    pub fn ensure(&self, host: &str) -> Result<ServerCert, String> {
        fs::create_dir_all(&self.dir).map_err(|e| error(&self.dir, e))?;
        fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| error(&self.dir, e))?;
        let (ca_key, ca_cert) = match self.read(CA_KEY, CA_CERT)? {
            Some(pair) => pair,
            None => {
                let (key, cert) = new_ca()?;
                self.write(CA_KEY, &key)?;
                self.write(CA_CERT, &cert)?;
                // A new authority: the old mirror certificate isn't signed by it.
                let _ = fs::remove_file(self.dir.join(SERVER_KEY));
                (key, cert)
            }
        };
        if let Some((key, cert)) = self.read(SERVER_KEY, SERVER_CERT)? {
            return Ok(ServerCert { cert, key });
        }
        let (key, cert) = new_server_cert(host, &ca_key, &ca_cert)?;
        self.write(SERVER_KEY, &key)?;
        self.write(SERVER_CERT, &cert)?;
        Ok(ServerCert { cert, key })
    }

    /// Both files, or `None` if either is missing.
    fn read(&self, key: &str, cert: &str) -> Result<Option<(String, String)>, String> {
        let read = |name: &str| {
            let path = self.dir.join(name);
            match fs::read_to_string(&path) {
                Ok(text) => Ok(Some(text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(error(&path, e)),
            }
        };
        Ok(read(key)?.zip(read(cert)?))
    }

    /// Private to the user: the folder holds the authority's key.
    fn write(&self, name: &str, text: &str) -> Result<(), String> {
        let path = self.dir.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| error(&path, e))?;
        file.write_all(text.as_bytes()).map_err(|e| error(&path, e))
    }
}

fn expiry_year() -> i32 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // The current year, near enough for a validity period.
    let year = 1970 + (seconds / 31_556_952) as i32;
    year + YEARS
}

fn new_ca() -> Result<(String, String), String> {
    let key = KeyPair::generate().map_err(|e| format!("mirror authority: {e}"))?;
    let mut params = CertificateParams::new(Vec::<String>::new())
        .map_err(|e| format!("mirror authority: {e}"))?;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params
        .distinguished_name
        .push(DnType::CommonName, "agent-run mirror authority");
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.not_after = rcgen::date_time_ymd(expiry_year(), 1, 1);
    let cert = params
        .self_signed(&key)
        .map_err(|e| format!("mirror authority: {e}"))?;
    Ok((key.serialize_pem(), cert.pem()))
}

fn new_server_cert(host: &str, ca_key: &str, ca_cert: &str) -> Result<(String, String), String> {
    let fail = |e: rcgen::Error| format!("mirror certificate: {e}");
    let issuer = Issuer::from_ca_cert_pem(ca_cert, KeyPair::from_pem(ca_key).map_err(fail)?)
        .map_err(fail)?;
    let key = KeyPair::generate().map_err(fail)?;
    let mut params = CertificateParams::new(vec![host.to_owned()]).map_err(fail)?;
    params.distinguished_name.push(DnType::CommonName, host);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.not_after = rcgen::date_time_ymd(expiry_year(), 1, 1);
    let cert = params.signed_by(&key, &issuer).map_err(fail)?;
    Ok((key.serialize_pem(), cert.pem()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_the_authority_once_and_keeps_its_key_private() {
        let dir = std::env::temp_dir().join(format!("mirror-tls-{}", std::process::id()));
        let tls = Tls::new(dir.clone());
        let first = tls.ensure("mirror").unwrap();
        assert!(first.cert.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(first.key.contains("PRIVATE KEY"));
        let ca = fs::read_to_string(tls.ca_cert()).unwrap();
        assert!(ca.starts_with("-----BEGIN CERTIFICATE-----"));
        assert_eq!(
            fs::metadata(dir.join(CA_KEY)).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // Kept: a new certificate would restart the mirror at every start.
        let again = tls.ensure("mirror").unwrap();
        assert_eq!(again.cert, first.cert);
        assert_eq!(fs::read_to_string(tls.ca_cert()).unwrap(), ca);
        // A new authority gets a new mirror certificate, signed by it.
        fs::remove_file(dir.join(CA_KEY)).unwrap();
        let renewed = tls.ensure("mirror").unwrap();
        assert_ne!(renewed.cert, first.cert);
        assert_ne!(fs::read_to_string(tls.ca_cert()).unwrap(), ca);
        fs::remove_dir_all(dir).unwrap();
    }
}
