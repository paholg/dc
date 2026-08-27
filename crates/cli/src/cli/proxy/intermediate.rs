//! Mint the short-lived, name-constrained intermediate CA the proxy uses.
//!
//! The root CA lives in `proxy.caRoot`, is generated on first use, and is read nowhere else. Both
//! the root and intermediate have X.509 name constraints limiting them to the TLDs in `proxy.tlds`:
//! trusting the root only extends trust for those suffixes.

use std::path::Path;

use eyre::{Result, WrapErr, bail, eyre};
use jiff::{SignedDuration, Timestamp};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, DnValue, GeneralSubtree, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, NameConstraints,
};
use shared::{ROOT_CA_KEY_PEM, ROOT_CA_PEM};

/// A fresh intermediate is minted at every proxy creation, so this is renewal
/// headroom, not a lifetime: [`expiry`] reports it due within [`RENEW_MARGIN`]
/// of the end, and `ensure_up` recreates the proxy on that signal.
pub(crate) const VALIDITY: SignedDuration = SignedDuration::from_hours(30 * 24);
pub(crate) const RENEW_MARGIN: SignedDuration = SignedDuration::from_hours(7 * 24);

/// Ten years, like mkcert's root: trusting it is a manual step, so it should be rare.
const ROOT_VALIDITY: SignedDuration = SignedDuration::from_hours(10 * 365 * 24);

/// How we recognize a root we generated. Anything else in `proxy.caRoot` is
/// rejected.
pub(crate) const ROOT_CN: &str = "devconcurrent root CA";

#[derive(Debug)]
pub(crate) struct Intermediate {
    pub(crate) cert_pem: String,
    pub(crate) key_pem: String,
    pub(crate) not_after: Timestamp,
}

/// Mint an intermediate CA signed by the root in `ca_root` — generating that
/// root first if the directory doesn't hold one — valid for [`VALIDITY`] and
/// name-constrained to `tlds` (each entry covers itself and all of its
/// subdomains).
pub(crate) fn mint(ca_root: &Path, tlds: &[String]) -> Result<Intermediate> {
    validate_tlds(tlds)?;
    let root = load_or_create_root(ca_root, tlds)?;

    let now = Timestamp::now();
    let not_after = now + VALIDITY;

    // No SANs: this is a CA cert, not a server cert.
    let mut params = CertificateParams::default();
    params.distinguished_name.push(
        DnType::CommonName,
        DnValue::Utf8String("devconcurrent intermediate CA".to_string()),
    );
    // pathlen 0: may sign leaves, never another CA.
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    // RFC 5280 permitted subtrees: dNSName "test" covers "test" and every
    // subdomain. rcgen marks the extension critical, as 5280 requires.
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: tlds.iter().cloned().map(GeneralSubtree::DnsName).collect(),
        excluded_subtrees: Vec::new(),
    });
    params.not_before = to_time(now - SignedDuration::from_mins(5));
    params.not_after = to_time(not_after);

    let key = KeyPair::generate().wrap_err("generate intermediate CA key")?;
    let cert = params
        .signed_by(&key, &root)
        .map_err(|e| eyre!("sign intermediate CA cert: {e}"))?;

    Ok(Intermediate {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
        not_after,
    })
}

/// Make sure the root CA in `dir` exists and matches `tlds`.
pub(crate) fn ensure_root(dir: &Path, tlds: &[String]) -> Result<()> {
    load_or_create_root(dir, tlds)?;

    Ok(())
}

/// Load the root CA, creating it if the directory doesn't hold one. An
/// existing root must be ours with name constraints matching `tlds`; anything
/// else errors.
///
/// This is a private method; callers that just want to ensure it exists should call `ensure_root`.
fn load_or_create_root(dir: &Path, tlds: &[String]) -> Result<Issuer<'static, KeyPair>> {
    validate_tlds(tlds)?;

    let cert_path = dir.join(ROOT_CA_PEM);
    let key_path = dir.join(ROOT_CA_KEY_PEM);

    match (cert_path.exists(), key_path.exists()) {
        (true, true) => {
            let cert_pem = std::fs::read_to_string(&cert_path)
                .wrap_err_with(|| format!("read {}", cert_path.display()))?;
            check_root(&cert_pem, &cert_path, tlds)?;
        }
        (false, false) => create_root(dir, &cert_path, &key_path, tlds)?,
        (cert, _) => {
            let (present, missing) = if cert {
                (&cert_path, &key_path)
            } else {
                (&key_path, &cert_path)
            };
            bail!(
                "{} exists but {} is missing; restore it, or move {} aside to have `dc proxy up` generate a fresh CA",
                present.display(),
                missing.display(),
                present.display(),
            );
        }
    }

    let cert_pem = std::fs::read_to_string(&cert_path)
        .wrap_err_with(|| format!("read {}", cert_path.display()))?;
    let key_pem = std::fs::read_to_string(&key_path)
        .wrap_err_with(|| format!("read {}", key_path.display()))?;
    let key = KeyPair::from_pem(&key_pem).wrap_err("parse root CA key")?;
    Issuer::from_ca_cert_pem(&cert_pem, key).map_err(|e| eyre!("parse root CA cert: {e}"))
}

fn create_root(dir: &Path, cert_path: &Path, key_path: &Path, tlds: &[String]) -> Result<()> {
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, DnValue::Utf8String(ROOT_CN.to_string()));
    params.distinguished_name.push(
        DnType::OrganizationName,
        DnValue::Utf8String("devconcurrent development CA".to_string()),
    );

    // pathlen 1: exactly deep enough for the intermediate the proxy runs on.
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(1));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    // The same constraints the intermediate gets: trusting this root only
    // extends trust for the configured TLDs.
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: tlds.iter().cloned().map(GeneralSubtree::DnsName).collect(),
        excluded_subtrees: Vec::new(),
    });
    let now = Timestamp::now();
    params.not_before = to_time(now - SignedDuration::from_mins(5));
    params.not_after = to_time(now + ROOT_VALIDITY);

    let key = KeyPair::generate().wrap_err("generate root CA key")?;
    let cert = params
        .self_signed(&key)
        .wrap_err("self-sign root CA cert")?;

    std::fs::create_dir_all(dir).wrap_err_with(|| format!("create {}", dir.display()))?;

    std::fs::write(cert_path, cert.pem())
        .wrap_err_with(|| format!("write {}", cert_path.display()))?;
    write_private(key_path, key.serialize_pem().as_bytes())
        .wrap_err_with(|| format!("write {}", key_path.display()))?;

    Ok(())
}

/// Check that an existing root is ours and constrained to exactly `tlds`
fn check_root(cert_pem: &str, path: &Path, tlds: &[String]) -> Result<()> {
    let path = path.display();

    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| eyre!("parse {path}: {e}"))?;
    let cert = pem.parse_x509().map_err(|e| eyre!("parse {path}: {e}"))?;

    let ours = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|cn| cn.as_str().ok())
        == Some(ROOT_CN);
    if !ours {
        bail!("The root CA at {path} was not generated by devconcurrent. Remove it and try again.");
    }

    let permitted: Vec<String> = cert
        .name_constraints()
        .map_err(|e| eyre!("parse name constraints of {path}: {e}"))?
        .and_then(|nc| nc.value.permitted_subtrees.as_ref())
        .map(|subtrees| {
            subtrees
                .iter()
                .filter_map(|sub| match &sub.base {
                    x509_parser::extensions::GeneralName::DNSName(name) => {
                        Some((*name).to_string())
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    if normalized(&permitted) != normalized(tlds) {
        let old = permitted.join(", ");
        let new = tlds.join(", ");
        bail!(
            "\
The allowed TLDs has changed from [{old}] to [{new}], and the CA needs to be regenerated.

1. Untrust and delete the old root: dc proxy untrust
2. Rerun this command. A fresh root will be generated and need to be re-trusted."
        );
    }

    Ok(())
}

/// dNSName comparison is case-insensitive, and the order entries were written
/// in doesn't matter.
fn normalized(tlds: &[String]) -> Vec<String> {
    let mut v: Vec<String> = tlds.iter().map(|t| t.to_ascii_lowercase()).collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

/// Where the proxy's intermediate stands relative to renewal, judged from the
/// RFC 3339 `notAfter` the CLI stamps on the container as a label.
pub(crate) enum Expiry {
    /// No label, or one we can't parse: a proxy created by an older CLI.
    Missing,
    Expired(Timestamp),
    /// Within [`RENEW_MARGIN`] of expiring.
    ExpiresSoon(Timestamp),
    Valid(Timestamp),
}

pub(crate) fn expiry(label: Option<&str>, now: Timestamp) -> Expiry {
    match label.and_then(|s| s.parse::<Timestamp>().ok()) {
        None => Expiry::Missing,
        Some(t) if t <= now => Expiry::Expired(t),
        Some(t) if t.duration_since(now) < RENEW_MARGIN => Expiry::ExpiresSoon(t),
        Some(t) => Expiry::Valid(t),
    }
}

/// A TLD here is any DNS suffix: dot-separated labels of `[A-Za-z0-9-]`.
fn validate_tlds(tlds: &[String]) -> Result<()> {
    if tlds.is_empty() {
        bail!("proxy.tlds is empty; you must specify at least one TLD");
    }

    for tld in tlds {
        let valid = !tld.is_empty()
            && tld.split('.').all(|label| {
                !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            });
        if !valid {
            bail!(
                "invalid entry {tld:?} in proxy.tlds: expected dot-separated labels of \
                 letters, digits, and hyphens, like \"test\" or \"internal.dev\""
            );
        }
    }
    Ok(())
}

fn to_time(ts: Timestamp) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(ts.as_second()).expect("timestamp in range")
}

#[cfg(test)]
mod tests {
    use x509_parser::prelude::{FromDer, GeneralName, ParsedExtension, X509Certificate};

    use super::*;

    fn fake_root(dir: &Path) {
        let mut params = CertificateParams::default();
        params.distinguished_name.push(
            DnType::CommonName,
            DnValue::Utf8String("test root".to_string()),
        );
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let key = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        std::fs::write(dir.join(ROOT_CA_PEM), cert.pem()).unwrap();
        std::fs::write(dir.join(ROOT_CA_KEY_PEM), key.serialize_pem()).unwrap();
    }

    #[test]
    fn mint_produces_constrained_ca() {
        let dir = tempfile::tempdir().unwrap();

        let tlds = vec!["test".to_string(), "internal.dev".to_string()];
        let minted = mint(dir.path(), &tlds).unwrap();

        let (_, der) = x509_parser::pem::parse_x509_pem(minted.cert_pem.as_bytes()).unwrap();
        let (_, cert) = X509Certificate::from_der(&der.contents).unwrap();

        let bc = cert.basic_constraints().unwrap().unwrap();
        assert!(bc.value.ca);
        assert_eq!(bc.value.path_len_constraint, Some(0));

        let nc = cert
            .iter_extensions()
            .find_map(|ext| match ext.parsed_extension() {
                ParsedExtension::NameConstraints(nc) => Some((ext.critical, nc)),
                _ => None,
            })
            .expect("name constraints present");
        assert!(nc.0, "name constraints must be critical");
        let permitted: Vec<_> =
            nc.1.permitted_subtrees
                .as_ref()
                .expect("permitted subtrees")
                .iter()
                .map(|sub| match &sub.base {
                    GeneralName::DNSName(name) => (*name).to_string(),
                    other => panic!("unexpected subtree {other:?}"),
                })
                .collect();
        assert_eq!(permitted, tlds);
        assert!(nc.1.excluded_subtrees.is_none());

        assert_eq!(
            cert.validity().not_after.timestamp(),
            minted.not_after.as_second()
        );
        let lifetime = minted.not_after.duration_since(Timestamp::now());
        assert!(lifetime <= VALIDITY);
        assert!(lifetime > VALIDITY - SignedDuration::from_hours(1));
    }

    /// The permitted DNS names in a cert's name constraints.
    fn permitted_dns(cert_pem: &str) -> Vec<String> {
        let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).unwrap();
        let root = pem.parse_x509().unwrap();
        root.name_constraints()
            .unwrap()
            .and_then(|nc| nc.value.permitted_subtrees.as_ref())
            .map(|subtrees| {
                subtrees
                    .iter()
                    .map(|sub| match &sub.base {
                        GeneralName::DNSName(name) => (*name).to_string(),
                        other => panic!("unexpected subtree {other:?}"),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn an_empty_ca_root_gets_a_generated_root() {
        let dir = tempfile::tempdir().unwrap();
        let ca_dir = dir.path().join("ca");

        let minted = mint(&ca_dir, &["test".to_string()]).unwrap();

        let cert_pem = std::fs::read_to_string(ca_dir.join(ROOT_CA_PEM)).unwrap();
        let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes()).unwrap();
        let root = pem.parse_x509().unwrap();
        let bc = root.basic_constraints().unwrap().unwrap();
        assert!(bc.value.ca);
        assert_eq!(bc.value.path_len_constraint, Some(1));
        assert_eq!(permitted_dns(&cert_pem), vec!["test".to_string()]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(ca_dir.join(ROOT_CA_KEY_PEM))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // The generated root signs a chain the intermediate verifies against.
        assert!(minted.cert_pem.contains("BEGIN CERTIFICATE"));
        // A second mint reuses the root rather than regenerating it.
        mint(&ca_dir, &["test".to_string()]).unwrap();
        assert_eq!(
            cert_pem,
            std::fs::read_to_string(ca_dir.join(ROOT_CA_PEM)).unwrap(),
        );
    }

    #[test]
    fn changing_tlds_errors_with_uninstall_instructions() {
        let dir = tempfile::tempdir().unwrap();
        mint(dir.path(), &["test".to_string()]).unwrap();
        let original = std::fs::read_to_string(dir.path().join(ROOT_CA_PEM)).unwrap();

        // Same set, different order and case: accepted.
        mint(dir.path(), &["TEST".to_string(), "test".to_string()]).unwrap();
        assert_eq!(
            original,
            std::fs::read_to_string(dir.path().join(ROOT_CA_PEM)).unwrap(),
        );

        // A different set: an error telling the user how to replace the root,
        // which is left in place so `dc proxy untrust` can still find it.
        let err = mint(dir.path(), &["dev".to_string(), "test".to_string()]).unwrap_err();
        assert!(err.to_string().contains("dc proxy untrust"), "{err}");
        assert_eq!(
            original,
            std::fs::read_to_string(dir.path().join(ROOT_CA_PEM)).unwrap(),
        );
    }

    #[test]
    fn a_foreign_root_is_rejected_with_guidance() {
        let dir = tempfile::tempdir().unwrap();
        fake_root(dir.path());
        let original = std::fs::read_to_string(dir.path().join(ROOT_CA_PEM)).unwrap();

        let err = mint(dir.path(), &["dev".to_string()]).unwrap_err();
        assert!(
            err.to_string()
                .contains("was not generated by devconcurrent"),
            "{err}"
        );
        // The root is left untouched.
        assert_eq!(
            original,
            std::fs::read_to_string(dir.path().join(ROOT_CA_PEM)).unwrap(),
        );
    }

    #[test]
    fn a_half_present_root_is_an_error_not_an_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(ROOT_CA_PEM), "not really a cert").unwrap();

        let err = mint(dir.path(), &["test".to_string()]).unwrap_err();
        assert!(err.to_string().contains("missing"), "{err}");
    }

    #[test]
    fn mint_rejects_bad_tlds() {
        let dir = tempfile::tempdir().unwrap();

        for bad in [vec![], vec![String::new()]] {
            assert!(mint(dir.path(), &bad).is_err());
        }
        for bad in ["*.test", ".test", "test.", "te st", "a..b"] {
            assert!(mint(dir.path(), &[bad.to_string()]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn expiry_thresholds() {
        let now: Timestamp = "2026-08-16T00:00:00Z".parse().unwrap();

        assert!(matches!(expiry(None, now), Expiry::Missing));
        assert!(matches!(expiry(Some("garbage"), now), Expiry::Missing));

        let expired = (now - SignedDuration::from_hours(1)).to_string();
        assert!(matches!(expiry(Some(&expired), now), Expiry::Expired(_)));

        let soon = (now + RENEW_MARGIN - SignedDuration::from_hours(1)).to_string();
        assert!(matches!(expiry(Some(&soon), now), Expiry::ExpiresSoon(_)));

        let valid = (now + VALIDITY).to_string();
        assert!(matches!(expiry(Some(&valid), now), Expiry::Valid(_)));
    }

    #[test]
    fn label_value_round_trips() {
        let ts = Timestamp::now();
        assert_eq!(ts.to_string().parse::<Timestamp>().unwrap(), ts);
    }
}
