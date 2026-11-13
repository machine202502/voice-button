//! Certificate for the phone server.
//!
//! Browsers only hand over the microphone in a secure context, so the site has
//! to speak HTTPS even on a home network. Nobody buys a certificate for
//! `192.168.1.10`, so the app keeps a tiny certificate authority of its own and
//! signs the server certificate with it.
//!
//! Two ways to use it:
//! - tap through the browser warning once (nothing to install), or
//! - install `ca.crt` on the phone once: then the address is trusted, the
//!   service worker registers and the page installs as a real app.
//!
//! The authority is created once and kept: the server certificate is re-signed
//! when the set of addresses changes (a new IP no longer matches the old SAN
//! list), and the phone keeps trusting it.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Validity of a generated certificate. Long enough not to bother the user,
/// short enough to be replaced eventually.
const VALID_DAYS: i64 = 825;

/// Validity of the authority itself: long, because re-installing it on the
/// phone is the one thing the user would have to notice.
const CA_DAYS: i64 = 3650;

/// What is on disk after `ensure`.
pub struct SiteCertificate {
    /// Server certificate followed by the authority, as a TLS chain.
    pub cert_pem: Vec<u8>,
    pub key_pem: Vec<u8>,
    /// The authority on its own, for the phone to install.
    pub ca_pem: Vec<u8>,
    /// True when the server certificate was (re)created during this call.
    pub regenerated: bool,
    /// Host names and addresses the certificate is valid for.
    pub names: Vec<String>,
}

impl SiteCertificate {
    /// Rustls configuration for the phone server.
    pub fn server_config(&self) -> Result<rustls::ServerConfig> {
        // The ring provider is compiled in; installing it once is enough and
        // harmless if another part of the app already did.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
            rustls_pemfile::certs(&mut &self.cert_pem[..])
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("certificate PEM is broken")?;
        let key = rustls_pemfile::private_key(&mut &self.key_pem[..])
            .context("key PEM is unreadable")?
            .context("key PEM has no private key")?;
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("rustls refused the certificate")?;
        Ok(config)
    }
}

/// Directory holding the certificate, the key and the record of what the
/// certificate covers.
fn dir(root: &Path) -> PathBuf {
    root.join("site").join("tls")
}

/// The authority that signs the server certificate. Kept across restarts and
/// across address changes: re-installing it on the phone is the one thing the
/// user would notice.
/// Returns the authority and the exact PEM the phone must install: the file on
/// disk, byte for byte. Re-serialising it would produce a certificate with the
/// same key but another signature, and the phone would not accept it as the
/// anchor it already trusts.
fn authority(dir: &Path) -> Result<(rcgen::Certificate, Vec<u8>)> {
    let cert_path = dir.join("ca.crt");
    let key_path = dir.join("ca.key");
    if cert_path.exists() && key_path.exists() {
        let pem = std::fs::read_to_string(&cert_path).context("cannot read the authority")?;
        let key_pem = std::fs::read_to_string(&key_path).context("cannot read the authority key")?;
        let key = rcgen::KeyPair::from_pem(&key_pem).context("the authority key is broken")?;
        let params = rcgen::CertificateParams::from_ca_cert_pem(&pem, key)
            .context("the authority certificate is broken")?;
        let certificate =
            rcgen::Certificate::from_params(params).context("cannot restore the authority")?;
        return Ok((certificate, pem.into_bytes()));
    }
    let mut params = rcgen::CertificateParams::new(Vec::new());
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Voice Button (local)");
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "Voice Button");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let now = chrono::Local::now();
    params.not_before = rcgen::date_time_ymd(now.year_floor(), 1, 1);
    let until = now + chrono::Duration::days(CA_DAYS);
    params.not_after = rcgen::date_time_ymd(until.year_floor(), until.month_floor(), until.day_floor());
    let cert = rcgen::Certificate::from_params(params).context("cannot create the authority")?;
    let pem = cert.serialize_pem()?.into_bytes();
    std::fs::write(&cert_path, &pem).context("cannot write the authority")?;
    std::fs::write(&key_path, cert.serialize_private_key_pem())
        .context("cannot write the authority key")?;
    crate::log::log("site: a certificate authority was created for the phone");
    Ok((cert, pem))
}

/// Certificate for `names` (addresses and host names), signed by the authority
/// and re-signed while it still matches.
pub fn ensure(root: &Path, names: &[String]) -> Result<SiteCertificate> {
    let dir = dir(root);
    std::fs::create_dir_all(&dir).context("cannot create the tls directory")?;
    let cert_path = dir.join("site.crt");
    let key_path = dir.join("site.key");
    let names_path = dir.join("names.txt");
    let ca_pem = std::fs::read(dir.join("ca.crt")).unwrap_or_default();

    let mut wanted: Vec<String> = names.iter().map(|name| name.trim().to_string()).collect();
    wanted.retain(|name| !name.is_empty());
    wanted.sort();
    wanted.dedup();
    wanted.push(format!("until={}", expiry_day()));

    let stored = std::fs::read_to_string(&names_path).unwrap_or_default();
    let up_to_date = cert_path.exists()
        && key_path.exists()
        && !ca_pem.is_empty()
        && stored.lines().map(str::to_string).collect::<Vec<_>>() == wanted;

    if up_to_date {
        let cert_pem = std::fs::read(&cert_path)?;
        let key_pem = std::fs::read(&key_path)?;
        return Ok(SiteCertificate {
            cert_pem,
            key_pem,
            ca_pem,
            regenerated: false,
            names: wanted
                .iter()
                .filter(|line| !line.starts_with("until="))
                .cloned()
                .collect(),
        });
    }

    let sans: Vec<String> = wanted
        .iter()
        .filter(|line| !line.starts_with("until="))
        .cloned()
        .collect();
    let (authority, ca_pem) = authority(&dir)?;
    let (cert_pem, key_pem) = generate(&sans, &authority, &ca_pem)?;
    std::fs::write(&cert_path, &cert_pem).context("cannot write the certificate")?;
    std::fs::write(&key_path, &key_pem).context("cannot write the private key")?;
    std::fs::write(&names_path, wanted.join("\n"))?;
    Ok(SiteCertificate {
        cert_pem,
        key_pem,
        ca_pem,
        regenerated: true,
        names: sans,
    })
}

/// Path of the authority certificate the phone may install.
pub fn authority_path(root: &Path) -> PathBuf {
    dir(root).join("ca.crt")
}

/// Drop the certificate: used when the phone server is switched off for good.
pub fn forget(root: &Path) {
    let _ = std::fs::remove_dir_all(dir(root));
}

/// Create a key and a certificate that covers `sans`, signed by `authority`.
fn generate(
    sans: &[String],
    authority: &rcgen::Certificate,
    authority_pem: &[u8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut params = rcgen::CertificateParams::new(sans.to_vec());
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Voice Button");
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "Voice Button");
    let now = chrono::Local::now();
    params.not_before = rcgen::date_time_ymd(now.year_floor(), 1, 1);
    let until = now + chrono::Duration::days(VALID_DAYS);
    params.not_after = rcgen::date_time_ymd(until.year_floor(), until.month_floor(), until.day_floor());

    let cert = rcgen::Certificate::from_params(params).context("cannot build the certificate")?;
    // The chain the phone receives: our certificate followed by its authority.
    let mut chain = cert
        .serialize_pem_with_signer(authority)
        .context("cannot sign the certificate")?;
    chain.push_str(&String::from_utf8_lossy(authority_pem));
    Ok((chain.into_bytes(), cert.serialize_private_key_pem().into_bytes()))
}

/// Last day the current certificate is valid, as recorded next to it.
fn expiry_day() -> String {
    (chrono::Local::now() + chrono::Duration::days(VALID_DAYS))
        .format("%Y-%m-%d")
        .to_string()
}

/// Small helpers so the validity dates read as dates, not magic numbers.
trait DateParts {
    fn year_floor(&self) -> i32;
    fn month_floor(&self) -> u8;
    fn day_floor(&self) -> u8;
}

impl DateParts for chrono::DateTime<chrono::Local> {
    fn year_floor(&self) -> i32 {
        chrono::Datelike::year(self)
    }
    fn month_floor(&self) -> u8 {
        chrono::Datelike::month(self) as u8
    }
    fn day_floor(&self) -> u8 {
        chrono::Datelike::day(self) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "voice-button-tls-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_certificate_is_created_and_then_reused() {
        let root = temp_root("create");
        let names = vec!["192.168.1.10".to_string(), "localhost".to_string()];
        let first = ensure(&root, &names).unwrap();
        assert!(first.regenerated);
        assert!(String::from_utf8_lossy(&first.cert_pem).contains("BEGIN CERTIFICATE"));
        assert!(String::from_utf8_lossy(&first.key_pem).contains("PRIVATE KEY"));
        assert!(first.server_config().is_ok());

        let second = ensure(&root, &names).unwrap();
        assert!(!second.regenerated);
        assert_eq!(second.cert_pem, first.cert_pem);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_new_address_regenerates_the_certificate() {
        let root = temp_root("change");
        let first = ensure(&root, &["192.168.1.10".to_string()]).unwrap();
        // Order does not matter, a new address does.
        let same = ensure(
            &root,
            &["192.168.1.10".to_string(), "localhost".to_string()],
        )
        .unwrap();
        assert!(same.regenerated);
        assert_ne!(same.cert_pem, first.cert_pem);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_authority_survives_new_addresses() {
        let root = temp_root("authority");
        let first = ensure(&root, &["192.168.1.10".to_string()]).unwrap();
        assert!(!first.ca_pem.is_empty());
        assert!(String::from_utf8_lossy(&first.ca_pem).contains("BEGIN CERTIFICATE"));
        let second = ensure(&root, &["10.0.0.5".to_string()]).unwrap();
        assert!(second.regenerated);
        // Re-installing the authority on the phone must not be needed again.
        assert_eq!(second.ca_pem, first.ca_pem);
        // The chain carries both the site certificate and its authority.
        let chain = String::from_utf8_lossy(&second.cert_pem);
        assert_eq!(chain.matches("BEGIN CERTIFICATE").count(), 2);
        assert!(authority_path(&root).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn forgetting_removes_the_files() {
        let root = temp_root("forget");
        ensure(&root, &["localhost".to_string()]).unwrap();
        forget(&root);
        assert!(!dir(&root).exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
