//! Browser (NSS) trust stores: Firefox profiles, plus on Linux the shared
//! `~/.pki/nssdb` that Chromium reads. These are user-owned databases
//! written with `certutil` from the NSS tools — no root needed.

use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::owo_colors::OwoColorize;
use eyre::{Result, eyre};

use super::Ca;

const CERTUTIL_HINT: &str = if cfg!(target_os = "macos") {
    "Install it with `brew install nss`."
} else if cfg!(target_os = "linux") {
    "Install it with your package manager: `apt install libnss3-tools`, `dnf install nss-tools`, `pacman -S nss`, or `zypper install mozilla-nss-tools`."
} else {
    "Install the NSS tools for your platform."
};

pub(super) fn install(ca: &Ca) -> Result<()> {
    let dbs = databases();
    if dbs.is_empty() {
        return Ok(());
    }
    let Some(certutil) = find_certutil() else {
        tracing::warn!(
            "browser (NSS) trust stores exist, but `certutil` isn't installed, so they were \
             skipped — Firefox won't trust the CA. {CERTUTIL_HINT}"
        );
        return Ok(());
    };

    let pem = ca.pem_path.display().to_string();
    let mut installed = 0usize;
    for db in &dbs {
        // -t C,,: a CA trusted to issue TLS server certs, nothing more.
        match run(
            &certutil,
            &["-A", "-d", db, "-t", "C,,", "-n", &ca.name, "-i", &pem],
        ) {
            Ok(()) => installed += 1,
            Err(e) => tracing::warn!("install into {db}: {e:#}"),
        }
    }
    if installed > 0 {
        tracing::info!(
            "{} installed in {installed} browser (NSS) trust store{}",
            "✓".green(),
            if installed == 1 { "" } else { "s" },
        );
    }
    Ok(())
}

pub(super) fn uninstall(ca: &Ca) -> Result<()> {
    let dbs = databases();
    if dbs.is_empty() {
        return Ok(());
    }
    let Some(certutil) = find_certutil() else {
        tracing::warn!(
            "browser (NSS) trust stores exist, but `certutil` isn't installed, so the CA \
             couldn't be removed from them. {CERTUTIL_HINT}"
        );
        return Ok(());
    };

    let mut removed = 0usize;
    for db in &dbs {
        // The legacy nickname covers a root installed by the old
        // `CAROOT=... mkcert -install` instructions.
        for name in [&ca.name, &ca.legacy_name] {
            if !contains(&certutil, db, name) {
                continue;
            }
            match run(&certutil, &["-D", "-d", db, "-n", name]) {
                Ok(()) => removed += 1,
                Err(e) => tracing::warn!("remove {name:?} from {db}: {e:#}"),
            }
        }
    }
    if removed > 0 {
        tracing::info!(
            "{} removed from {removed} browser (NSS) trust store{}",
            "✓".green(),
            if removed == 1 { "" } else { "s" },
        );
    }
    Ok(())
}

fn contains(certutil: &Path, db: &str, name: &str) -> bool {
    Command::new(certutil)
        .args(["-L", "-d", db, "-n", name])
        .output()
        .is_ok_and(|out| out.status.success())
}

fn run(certutil: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new(certutil)
        .args(args)
        .output()
        .map_err(|e| eyre!("run {}: {e}", certutil.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(eyre!(
            "`certutil {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim(),
        ))
    }
}

/// `certutil -d` specs (`sql:<dir>` / `dbm:<dir>`) for every NSS database on
/// this machine.
fn databases() -> Vec<String> {
    let Some(base) = directories::BaseDirs::new() else {
        return Vec::new();
    };

    databases_under(base.home_dir(), base.config_dir())
}

fn databases_under(home: &Path, config: &Path) -> Vec<String> {
    let mut dirs = fixed_dirs(home);
    for parent in profile_parents(home, config) {
        let Ok(entries) = std::fs::read_dir(parent) else {
            continue;
        };
        dirs.extend(entries.flatten().map(|e| e.path()));
    }
    dirs.iter().filter_map(|dir| spec(dir)).collect()
}

/// How `certutil -d` addresses the database in `dir`, or `None` if there
/// isn't one: `sql:` for the current SQLite format, `dbm:` for the legacy
/// Berkeley DB one.
fn spec(dir: &Path) -> Option<String> {
    if dir.join("cert9.db").exists() {
        Some(format!("sql:{}", dir.display()))
    } else if dir.join("cert8.db").exists() {
        Some(format!("dbm:{}", dir.display()))
    } else {
        None
    }
}

/// Databases at fixed locations, present or not.
fn fixed_dirs(home: &Path) -> Vec<PathBuf> {
    if cfg!(target_os = "linux") {
        vec![
            home.join(".pki/nssdb"),
            home.join("snap/chromium/current/.pki/nssdb"),
            PathBuf::from("/etc/pki/nssdb"),
        ]
    } else {
        Vec::new()
    }
}

/// Directories whose immediate children may be Firefox profiles.
fn profile_parents(home: &Path, config: &Path) -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![home.join("Library/Application Support/Firefox/Profiles")]
    } else if cfg!(target_os = "linux") {
        vec![
            home.join(".mozilla/firefox"),
            // Firefox 147+ uses XDG_CONFIG_DIR:
            config.join("mozilla/firefox"),
            home.join("snap/firefox/common/.mozilla/firefox"),
            // For flatpak:
            home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
        ]
    } else {
        Vec::new()
    }
}

/// The path to `certutil` from the NSS tools, if it can be found.
fn find_certutil() -> Option<PathBuf> {
    if let Ok(found) = which::which("certutil") {
        return Some(found);
    }

    if cfg!(target_os = "macos") {
        // Homebrew's nss is keg-only — never linked into PATH — so probe
        // the standard keg locations, then ask brew itself.
        for dir in ["/opt/homebrew/opt/nss/bin", "/usr/local/opt/nss/bin"] {
            let candidate = Path::new(dir).join("certutil");
            if candidate.is_file() {
                return Some(candidate);
            }
        }

        if let Ok(out) = Command::new("brew").args(["--prefix", "nss"]).output()
            && out.status.success()
        {
            let prefix = String::from_utf8_lossy(&out.stdout);
            let candidate = Path::new(prefix.trim()).join("bin/certutil");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_recognized_by_their_cert_database() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let config = home.join(".config");

        let parents = profile_parents(home, &config);
        let current = parents[0].join("abc123.default");
        let legacy = parents[0].join("old.profile");
        let empty = parents[0].join("no-db-here");
        // A Firefox 147+ profile under the XDG config dir.
        let xdg = parents.get(1).map(|p| p.join("xdg.default"));
        for dir in [&current, &legacy, &empty].into_iter().chain(xdg.as_ref()) {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(current.join("cert9.db"), "").unwrap();
        std::fs::write(legacy.join("cert8.db"), "").unwrap();
        if let Some(xdg) = &xdg {
            std::fs::write(xdg.join("cert9.db"), "").unwrap();
        }

        // Only databases under the fake home: the machine running the tests
        // may have real ones at the fixed absolute paths.
        let home_str = home.display().to_string();
        let dbs: Vec<String> = databases_under(home, &config)
            .into_iter()
            .filter(|db| db.contains(&home_str))
            .collect();

        let expected = 2 + usize::from(xdg.is_some());
        assert_eq!(dbs.len(), expected, "{dbs:?}");
        assert!(
            dbs.contains(&format!("sql:{}", current.display())),
            "{dbs:?}"
        );
        assert!(
            dbs.contains(&format!("dbm:{}", legacy.display())),
            "{dbs:?}"
        );
        if let Some(xdg) = &xdg {
            assert!(dbs.contains(&format!("sql:{}", xdg.display())), "{dbs:?}");
        }
    }
}
