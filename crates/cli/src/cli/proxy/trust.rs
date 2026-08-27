//! Installs the root CA into the system and browser trust stores, and
//! removes it again — the job the old instructions delegated to
//! `mkcert -install` / `mkcert -uninstall`.

mod nss;
mod sudo;
mod system;

use std::path::{Path, PathBuf};

use clap::Args;
use color_eyre::owo_colors::OwoColorize;
use eyre::{Result, WrapErr, eyre};
use shared::{ROOT_CA_KEY_PEM, ROOT_CA_PEM};

use crate::cli::proxy::intermediate;
use crate::config::Config;

#[derive(Debug, Args)]
pub(crate) struct Trust;

#[derive(Debug, Args)]
pub(crate) struct Untrust;

impl Trust {
    pub(crate) fn run(self) -> Result<()> {
        let config = Config::load()?;
        let dir = config.proxy.ca_root_dir()?;
        intermediate::ensure_root(&dir, &config.proxy.tlds)?;
        let ca = Ca::load(&dir)?;

        // Browser stores first: they never fail hard, so on a machine with no
        // writable system store (NixOS) the browsers still get trust before
        // the error explains the manual step.
        nss::install(&ca)?;
        system::install(&ca)
    }
}

impl Untrust {
    pub(crate) fn run(self) -> Result<()> {
        let config = Config::load()?;
        let dir = config.proxy.ca_root_dir()?;
        let ca = Ca::load(&dir)
            .wrap_err("untrusting needs the root's cert file, to identify it in the stores")?;

        nss::uninstall(&ca)?;
        system::uninstall(&ca)?;

        // Only after every store let go of it: the file is how the stores'
        // entries are identified.
        for name in [ROOT_CA_PEM, ROOT_CA_KEY_PEM] {
            let path = dir.join(name);
            if let Err(e) = std::fs::remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                return Err(eyre!("delete {}: {e}", path.display()));
            }
        }
        tracing::info!(
            "{} deleted the CA files; the next `dc proxy up` or `dc proxy trust` generates a fresh CA",
            "✓".green(),
        );
        Ok(())
    }
}

/// The on-disk root CA, plus the names that identify it inside trust stores.
struct Ca {
    pem_path: PathBuf,
    /// NSS nickname and anchor file stem: `devconcurrent development CA
    /// <serial>`. Stable for a given root, distinct across regenerations.
    name: String,
    /// What `mkcert -install` called this same root: its naming scheme, our
    /// serial.
    legacy_name: String,
}

impl Ca {
    fn load(dir: &Path) -> Result<Self> {
        let pem_path = dir.join(ROOT_CA_PEM);
        let pem = std::fs::read_to_string(&pem_path)
            .wrap_err_with(|| format!("read {}", pem_path.display()))?;
        let serial = serial(&pem).wrap_err_with(|| format!("parse {}", pem_path.display()))?;
        Ok(Self {
            name: format!("devconcurrent development CA {serial}"),
            legacy_name: format!("mkcert development CA {serial}"),
            pem_path,
        })
    }
}

/// The root's serial number, in decimal.
fn serial(cert_pem: &str) -> Result<String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| eyre!("parse PEM: {e}"))?;
    let cert = pem
        .parse_x509()
        .map_err(|e| eyre!("parse certificate: {e}"))?;
    // rcgen derives the serial from the key when none is set, so it is
    // unique per generated root; mkcert put the same value in its names.
    Ok(cert.tbs_certificate.serial.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_names_come_from_the_roots_serial() {
        let dir = tempfile::tempdir().unwrap();
        intermediate::ensure_root(dir.path(), &["test".to_string()]).unwrap();

        let ca = Ca::load(dir.path()).unwrap();
        let serial = ca
            .name
            .strip_prefix("devconcurrent development CA ")
            .expect("our naming scheme");
        assert!(!serial.is_empty());
        assert!(serial.chars().all(|c| c.is_ascii_digit()), "{serial}");
        // Same serial under mkcert's scheme, so roots installed with the old
        // instructions can be found and removed.
        assert_eq!(ca.legacy_name, format!("mkcert development CA {serial}"));

        // The names are read from disk, not generated: loading again agrees.
        assert_eq!(Ca::load(dir.path()).unwrap().name, ca.name);
    }

    #[test]
    fn a_missing_root_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Ca::load(dir.path()).is_err());
    }
}
