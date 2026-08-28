//! The platform trust store: the macOS System keychain, or a Linux distro's
//! CA anchor directory plus the command that rebuilds the bundle from it —
//! and, under WSL, additionally the Windows store that native browsers read.

use eyre::{Result, bail};

use super::Ca;

pub(super) fn install(ca: &Ca) -> Result<()> {
    if cfg!(target_os = "macos") {
        macos::install(ca)
    } else if cfg!(target_os = "linux") {
        // Both stores matter under WSL: the distro's for tools inside it,
        // Windows' for the browsers outside it.
        linux::install(ca)?;
        if wsl() {
            windows::install(ca)?;
        }
        Ok(())
    } else {
        bail!("only the Linux and macOS system trust stores are supported")
    }
}

pub(super) fn uninstall(ca: &Ca) -> Result<()> {
    if cfg!(target_os = "macos") {
        macos::uninstall(ca)
    } else if cfg!(target_os = "linux") {
        linux::uninstall(ca)?;
        if wsl() {
            windows::uninstall()?;
        }
        Ok(())
    } else {
        bail!("only the Linux and macOS system trust stores are supported")
    }
}

/// Whether this Linux is a WSL distro, with Windows itself alongside.
pub(crate) fn wsl() -> bool {
    cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/version").is_ok_and(|v| is_wsl_version(&v))
}

fn is_wsl_version(version: &str) -> bool {
    version.to_ascii_lowercase().contains("microsoft")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_is_recognized_by_its_kernel_version() {
        assert!(is_wsl_version(
            "Linux version 5.15.167.4-microsoft-standard-WSL2 (root@f9c826d3017f) ..."
        ));
        // WSL1 spells it with a capital M.
        assert!(is_wsl_version(
            "Linux version 4.4.0-19041-Microsoft (Microsoft@Microsoft.com) ..."
        ));
        assert!(!is_wsl_version(
            "Linux version 6.12.4-arch1-1 (linux@archlinux) ..."
        ));
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

pub(crate) mod windows {
    //! The Windows current-user Root store, reached from inside WSL: Windows
    //! binaries are callable here, so `certutil.exe` can install the CA where
    //! native browsers (Chrome, Edge) look. No elevation needed — Windows
    //! asks the user to confirm in a security dialog instead.
    //!
    //! The certutil driving lives in the `winstore` crate, which also
    //! compiles on Windows so CI can exercise it against a real store; this
    //! module adds what's WSL-specific — finding certutil.exe across interop
    //! and translating the CA's path.

    use std::path::{Path, PathBuf};
    use std::process::Command;

    use color_eyre::owo_colors::OwoColorize;
    use eyre::{Result, bail, eyre};
    pub(crate) use winstore::store_contains;

    use super::super::Ca;
    use crate::cli::proxy::intermediate::ROOT_CN;

    pub(super) fn install(ca: &Ca) -> Result<()> {
        let Some(certutil) = find_certutil() else {
            bail!(
                "\
certutil.exe can't be found, so the CA can't reach the Windows certificate store that native
browsers read. Enable WSL interop, or install it from Windows yourself:
`certutil -user -addstore Root <path to rootCA.pem>`"
            );
        };
        let path = windows_path(&ca.pem_path)?;
        tracing::info!(
            "Windows will ask you to confirm adding the CA to your user's certificate store"
        );
        winstore::install(&certutil, winstore::Scope::User, &path)?;
        tracing::info!("{} installed in the Windows certificate store", "✓".green());
        Ok(())
    }

    pub(super) fn uninstall() -> Result<()> {
        let Some(certutil) = find_certutil() else {
            bail!(
                "\
certutil.exe can't be found, so the CA can't be removed from the Windows certificate store.
Enable WSL interop, or remove it from Windows yourself:
`certutil -user -delstore Root \"{ROOT_CN}\"`"
            );
        };
        if winstore::uninstall(&certutil, winstore::Scope::User, ROOT_CN)? == 0 {
            tracing::info!("not in the Windows certificate store; nothing to remove");
        } else {
            tracing::info!("{} removed from the Windows certificate store", "✓".green());
        }
        Ok(())
    }

    /// The certutil dump of every devconcurrent root in the user's Root
    /// store; see [`winstore::root_store`].
    pub(crate) fn user_root_store() -> Result<String> {
        let certutil = find_certutil()
            .ok_or_else(|| eyre!("certutil.exe can't be found (is WSL interop enabled?)"))?;
        winstore::root_store(&certutil, winstore::Scope::User, ROOT_CN)
    }

    /// Windows' own certificate tool (unrelated to the NSS `certutil`).
    /// Interop puts the Windows PATH on ours by default; the fixed path
    /// covers setups with that appending turned off.
    fn find_certutil() -> Option<PathBuf> {
        if let Ok(found) = which::which("certutil.exe") {
            return Some(found);
        }
        let fixed = Path::new("/mnt/c/Windows/System32/certutil.exe");
        fixed.is_file().then(|| fixed.to_path_buf())
    }

    /// How Windows addresses `path`: the `\\wsl.localhost\...` UNC form from
    /// `wslpath`, which every WSL distro ships and certutil accepts.
    fn windows_path(path: &Path) -> Result<String> {
        let out = Command::new("wslpath")
            .arg("-w")
            .arg(path)
            .output()
            .map_err(|e| eyre!("run wslpath: {e}"))?;
        if !out.status.success() {
            bail!(
                "`wslpath -w {}` failed: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim(),
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}
