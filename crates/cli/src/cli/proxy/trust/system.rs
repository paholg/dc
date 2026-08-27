//! The platform trust store: the macOS System keychain, or a Linux distro's
//! CA anchor directory plus the command that rebuilds the bundle from it.

use eyre::{Result, bail};

use super::Ca;

pub(super) fn install(ca: &Ca) -> Result<()> {
    if cfg!(target_os = "macos") {
        macos::install(ca)
    } else if cfg!(target_os = "linux") {
        linux::install(ca)
    } else {
        bail!("only the Linux and macOS system trust stores are supported")
    }
}

pub(super) fn uninstall(ca: &Ca) -> Result<()> {
    if cfg!(target_os = "macos") {
        macos::uninstall(ca)
    } else if cfg!(target_os = "linux") {
        linux::uninstall(ca)
    } else {
        bail!("only the Linux and macOS system trust stores are supported")
    }
}

mod linux {
    use std::path::{Path, PathBuf};

    use color_eyre::owo_colors::OwoColorize;
    use eyre::{Result, bail};

    use super::super::{Ca, sudo};

    /// A distro family's CA layout.
    struct Store {
        // Present exactly on distros using this layout; relative so tests
        // can probe under a temp root.
        anchors: &'static str,
        ext: &'static str,
        update: &'static [&'static str],
    }

    const STORES: &[Store] = &[
        // Fedora, RHEL
        Store {
            anchors: "etc/pki/ca-trust/source/anchors",
            ext: "pem",
            update: &["update-ca-trust", "extract"],
        },
        // Debian, Ubuntu
        Store {
            anchors: "usr/local/share/ca-certificates",
            ext: "crt",
            update: &["update-ca-certificates"],
        },
        // Arch
        Store {
            anchors: "etc/ca-certificates/trust-source/anchors",
            ext: "crt",
            update: &["trust", "extract-compat"],
        },
        // openSUSE
        Store {
            anchors: "usr/share/pki/trust/anchors",
            ext: "pem",
            update: &["update-ca-certificates"],
        },
    ];

    fn detect(root: &Path) -> Option<&'static Store> {
        STORES.iter().find(|s| root.join(s.anchors).is_dir())
    }

    fn anchor_path(root: &Path, store: &Store, name: &str) -> PathBuf {
        root.join(store.anchors)
            .join(format!("{}.{}", name.replace(' ', "_"), store.ext))
    }

    pub(super) fn install(ca: &Ca) -> Result<()> {
        let root = Path::new("/");
        let Some(store) = detect(root) else {
            bail!(
                "\
No known system CA anchor directory exists on this machine

See your distribution's documentation for adding it as a trusted root."
            );
        };

        let src = ca.pem_path.display().to_string();
        let dest = anchor_path(root, store, &ca.name).display().to_string();
        sudo::run_root(&["cp", &src, &dest])?;
        sudo::run_root(store.update)?;
        tracing::info!("{} system trust store: installed {dest}", "✓".green());
        Ok(())
    }

    pub(super) fn uninstall(ca: &Ca) -> Result<()> {
        let root = Path::new("/");
        let Some(store) = detect(root) else {
            tracing::info!("no system CA anchor directory on this machine; nothing to remove");
            return Ok(());
        };

        // The legacy name covers a root installed by the old
        // `CAROOT=... mkcert -install` instructions; -f makes removing
        // anchors that were never installed fine.
        let ours = anchor_path(root, store, &ca.name).display().to_string();
        let legacy = anchor_path(root, store, &ca.legacy_name)
            .display()
            .to_string();
        sudo::run_root(&["rm", "-f", &ours, &legacy])?;
        sudo::run_root(store.update)?;
        tracing::info!("{} system trust store: removed", "✓".green());
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_first_existing_anchor_directory_wins() {
            let root = tempfile::tempdir().unwrap();
            assert!(detect(root.path()).is_none());

            std::fs::create_dir_all(root.path().join("usr/local/share/ca-certificates")).unwrap();
            let store = detect(root.path()).unwrap();
            assert_eq!(store.update, &["update-ca-certificates"]);

            // Fedora's layout comes first in the table.
            std::fs::create_dir_all(root.path().join("etc/pki/ca-trust/source/anchors")).unwrap();
            let store = detect(root.path()).unwrap();
            assert_eq!(store.update, &["update-ca-trust", "extract"]);
        }

        #[test]
        fn anchor_files_get_underscores_and_the_stores_extension() {
            let store = &STORES[1]; // Debian
            assert_eq!(
                anchor_path(Path::new("/"), store, "devconcurrent development CA 42"),
                Path::new("/usr/local/share/ca-certificates/devconcurrent_development_CA_42.crt"),
            );
        }
    }
}

mod macos {
    use std::process::Command;

    use color_eyre::owo_colors::OwoColorize;
    use eyre::Result;

    use super::super::{Ca, sudo};
    use crate::cli::proxy::intermediate::ROOT_CN;

    const SYSTEM_KEYCHAIN: &str = "/Library/Keychains/System.keychain";

    pub(super) fn install(ca: &Ca) -> Result<()> {
        // Since macOS 15, changing trust settings can raise a confirmation
        // dialog that sudo doesn't cover.
        tracing::info!("macOS may ask you to confirm this change in a security dialog");
        let pem = ca.pem_path.display().to_string();
        sudo::run_root(&[
            "security",
            "add-trusted-cert",
            "-d",
            "-k",
            SYSTEM_KEYCHAIN,
            &pem,
        ])?;
        tracing::info!("{} installed in the system keychain", "✓".green());
        Ok(())
    }

    pub(super) fn uninstall(ca: &Ca) -> Result<()> {
        // Gate on presence so a never-trusted root uninstalls cleanly, but a
        // real removal failure is an error: untrust deletes the cert file
        // afterwards, and deleting it while still trusted would strand the
        // keychain entry with nothing to identify it by.
        if !installed() {
            tracing::info!("not in the system keychain; nothing to remove");
            return Ok(());
        }
        let pem = ca.pem_path.display().to_string();
        sudo::run_root(&["security", "remove-trusted-cert", "-d", &pem])?;
        tracing::info!("{} removed from the system keychain", "✓".green());
        Ok(())
    }

    /// Whether any devconcurrent root is in the system keychain.
    fn installed() -> bool {
        // Reading the keychain needs no sudo.
        Command::new("security")
            .args(["find-certificate", "-c", ROOT_CN, SYSTEM_KEYCHAIN])
            .output()
            .is_ok_and(|out| out.status.success())
    }
}
