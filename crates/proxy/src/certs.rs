//! Intermediate CA delivered by the CLI at proxy creation, used to mint
//! per-service leaf certs on demand. The root CA's key never enters any
//! container; this process holds only a short-lived intermediate that is
//! name-constrained to the configured TLDs, so a compromise here cannot mint
//! certs for anything outside them.
//!
//! Sidecars don't trust each other and don't hold any CA key: they receive
//! only the leaf key plus the leaf + intermediate cert chain.

use std::path::Path;
use std::sync::Arc;

use eyre::{Context, Result, eyre};
use rcgen::{CertificateParams, Issuer, KeyPair, KeyUsagePurpose};
use shared::{PROXY_CA_CERT_FILE, PROXY_CA_KEY_FILE};

/// Holds the intermediate CA cert + key in memory. Cheap to clone.
#[derive(Clone)]
pub struct CaHolder {
    issuer: Arc<Issuer<'static, KeyPair>>,
    /// The intermediate's own PEM, appended to every minted leaf so sidecars
    /// serve the full chain (clients only trust the root CA).
    chain_pem: Arc<str>,
    /// The intermediate's `notAfter`, which caps every leaf we mint.
    not_after: time::OffsetDateTime,
}

impl CaHolder {
    /// Load `intermediateCA.pem` + `intermediateCA-key.pem` from `dir`.
    pub fn load(dir: &Path) -> Result<Self> {
        let cert_pem = std::fs::read_to_string(dir.join(PROXY_CA_CERT_FILE))
            .wrap_err_with(|| format!("read {}", dir.join(PROXY_CA_CERT_FILE).display()))?;
        let key_pem = std::fs::read_to_string(dir.join(PROXY_CA_KEY_FILE))
            .wrap_err_with(|| format!("read {}", dir.join(PROXY_CA_KEY_FILE).display()))?;
        let key = KeyPair::from_pem(&key_pem).wrap_err("parse CA key")?;
        let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
            .map_err(|e| eyre!("parse CA cert PEM: {e}"))?;
        let not_after = pem
            .parse_x509()
            .map_err(|e| eyre!("parse CA cert: {e}"))?
            .validity()
            .not_after
            .timestamp();
        let not_after = time::OffsetDateTime::from_unix_timestamp(not_after)
            .wrap_err("CA cert notAfter out of range")?;
        let issuer = Issuer::from_ca_cert_pem(&cert_pem, key).wrap_err("parse CA cert")?;
        Ok(Self {
            issuer: Arc::new(issuer),
            chain_pem: cert_pem.into(),
            not_after,
        })
    }

    /// Mint a leaf cert signed by the loaded CA, with SANs for `hostname` and
    /// the one-level wildcard `*.hostname` so direct subdomains resolve over
    /// TLS too. Returns `(cert_pem, key_pem)`, where the cert PEM is the full
    /// chain: leaf first, then the intermediate.
    //
    // If we ever need to support more levels than 1 subdomain, we'll have to mint certs on-demand,
    // or provide a place to pre-configure them.
    pub fn mint(&self, hostname: &str) -> Result<(String, String)> {
        let wildcard = format!("*.{hostname}");
        let mut params = CertificateParams::new(vec![hostname.to_string(), wildcard])
            .wrap_err("build leaf cert params")?;
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            rcgen::DnValue::Utf8String(hostname.to_string()),
        );
        params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        params.key_usages.push(KeyUsagePurpose::KeyEncipherment);
        params
            .extended_key_usages
            .push(rcgen::ExtendedKeyUsagePurpose::ServerAuth);
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::minutes(5);
        params.not_after = self.not_after;
        let leaf_key = KeyPair::generate().wrap_err("generate leaf key")?;
        let leaf = params
            .signed_by(&leaf_key, &self.issuer)
            .map_err(|e| eyre!("sign leaf cert: {e}"))?;
        let chain = format!("{}\n{}", leaf.pem().trim_end(), self.chain_pem);
        Ok((chain, leaf_key.serialize_pem()))
    }
}

#[cfg(test)]
mod tests {
    use rcgen::{BasicConstraints, IsCa};

    use super::*;

    #[test]
    fn minted_leaf_validity_matches_the_intermediate() {
        let dir = tempfile::tempdir().unwrap();
        let now = time::OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .unwrap();
        let ca_not_after = now + time::Duration::days(30);

        // A self-signed CA standing in for the intermediate.
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.not_before = now - time::Duration::minutes(5);
        params.not_after = ca_not_after;
        let key = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        std::fs::write(dir.path().join(PROXY_CA_CERT_FILE), cert.pem()).unwrap();
        std::fs::write(dir.path().join(PROXY_CA_KEY_FILE), key.serialize_pem()).unwrap();

        let ca = CaHolder::load(dir.path()).unwrap();
        let (chain_pem, _key_pem) = ca.mint("svc.test").unwrap();

        let (_, pem) = x509_parser::pem::parse_x509_pem(chain_pem.as_bytes()).unwrap();
        let leaf = pem.parse_x509().unwrap();
        let not_before = leaf.validity().not_before.timestamp();
        let not_after = leaf.validity().not_after.timestamp();

        assert_eq!(not_after, ca_not_after.unix_timestamp());
        assert!(not_before <= now.unix_timestamp());
        assert!(not_before >= (now - time::Duration::minutes(10)).unix_timestamp());
        // Apple rejects TLS server certs valid longer than 825 days.
        assert!(not_after - not_before <= 825 * 24 * 60 * 60);
    }
}
