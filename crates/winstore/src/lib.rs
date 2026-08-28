//! The Windows Root certificate stores, driven through `certutil.exe` —
//! where native browsers (Chrome, Edge) look for trusted roots.
//!
//! Split out of the CLI so it also compiles on Windows: the CLI calls it
//! from inside WSL, where Windows binaries are callable, and the
//! `winstore-e2e` binary runs the same code natively in CI, which can't
//! run WSL.
//! Locating certutil.exe and translating paths across the WSL boundary are
//! the caller's job — everything here takes the binary and a path certutil
//! can already open.

use std::path::Path;
use std::process::Command;

use eyre::{Result, eyre};

/// Which Root store to operate on. The CLI uses `User`: no elevation, and
/// Windows asks the user to confirm installs in a security dialog. That
/// dialog waits for a click forever on a CI runner, so the e2e binary uses
/// `Machine`, where addstore needs admin instead of a dialog — the runner
/// is admin, and the commands are otherwise identical.
#[derive(Clone, Copy)]
pub enum Scope {
    User,
    Machine,
}

impl Scope {
    /// The scope-selecting flags certutil takes before the verb.
    fn flags(self) -> &'static [&'static str] {
        match self {
            Scope::User => &["-user"],
            Scope::Machine => &[],
        }
    }
}

fn command(certutil: &Path, scope: Scope, rest: &[&str]) -> Command {
    let mut cmd = Command::new(certutil);
    cmd.args(scope.flags()).args(rest);
    cmd
}

/// Add the certificate at `cert_path` (PEM; a path certutil can open, so
/// under WSL the `wslpath -w` form) to the Root store. For [`Scope::User`],
/// Windows asks the user to confirm in a security dialog; declining fails.
pub fn install(certutil: &Path, scope: Scope, cert_path: &str) -> Result<()> {
    run(certutil, scope, &["-addstore", "Root", cert_path])
}

/// Remove every certificate with `cn` from the Root store, returning how
/// many delstore passes it took (0 when there were none).
pub fn uninstall(certutil: &Path, scope: Scope, cn: &str) -> Result<usize> {
    // delstore removes one match per call, and re-trusting a regenerated
    // root without untrusting first leaves several under the same CN.
    let mut removed = 0usize;
    while installed(certutil, scope, cn) {
        if removed > 32 {
            return Err(eyre!(
                "the certificate is still in the store after removing it"
            ));
        }
        run(certutil, scope, &["-delstore", "Root", cn])?;
        removed += 1;
    }
    Ok(removed)
}

/// The certutil dump of every `cn` certificate in the Root store — empty
/// when there are none. `Err` means the store couldn't be asked at all,
/// which proves nothing about trust.
pub fn root_store(certutil: &Path, scope: Scope, cn: &str) -> Result<String> {
    let out = command(certutil, scope, &["-store", "Root", cn])
        .output()
        .map_err(|e| eyre!("run {}: {e}", certutil.display()))?;
    // certutil fails when nothing matches the CN: an empty store, not an
    // unreadable one.
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Ok(String::new())
    }
}

/// Whether a [`root_store`] dump lists the certificate in `cert_der`,
/// matched by serial number — the CN alone could be a stale root from
/// before a regeneration.
pub fn store_contains(dump: &str, cert_der: &[u8]) -> bool {
    let Ok((_, cert)) = x509_parser::parse_x509_certificate(cert_der) else {
        return false;
    };
    let hex: String = cert
        .tbs_certificate
        .raw_serial()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    store_dump_contains(dump, &hex)
}

/// certutil labels the serial in the system language, so look for the hex
/// itself rather than a "Serial Number:" line — long enough that a match
/// can't be an accident.
fn store_dump_contains(dump: &str, serial_hex: &str) -> bool {
    let dump = dump.to_ascii_lowercase();
    let full = serial_hex.to_ascii_lowercase();
    // certutil may print the serial without DER's sign-padding zeros.
    let trimmed = full.trim_start_matches("00");
    dump.contains(&full) || (!trimmed.is_empty() && dump.contains(trimmed))
}

/// Whether any `cn` certificate is in the Root store.
fn installed(certutil: &Path, scope: Scope, cn: &str) -> bool {
    // -store just dumps matches, where -verifystore would also
    // chain-validate and can fail for unrelated reasons.
    command(certutil, scope, &["-store", "Root", cn])
        .output()
        .is_ok_and(|out| out.status.success())
}

fn run(certutil: &Path, scope: Scope, args: &[&str]) -> Result<()> {
    let out = command(certutil, scope, args)
        .output()
        .map_err(|e| eyre!("run {}: {e}", certutil.display()))?;
    if out.status.success() {
        return Ok(());
    }
    // certutil reports errors on stdout.
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let why = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    Err(eyre!(
        "`certutil.exe {} {}` failed: {}",
        scope.flags().join(" "),
        args.join(" "),
        why.trim(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_is_found_in_a_store_dump_by_its_serial() {
        // The high bit is set, so the DER serial carries a sign-padding zero.
        let serial = [0xABu8, 0xCD, 0xEF, 0x12, 0x34, 0x56, 0x78, 0x90];
        let mut params = rcgen::CertificateParams::default();
        params.serial_number = Some(rcgen::SerialNumber::from_slice(&serial));
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let der = cert.der().as_ref();

        // A dump in certutil's shape, though matching doesn't depend on the
        // labels (they're localized).
        let dump = "\
            Root \"Trusted Root Certification Authorities\"\n\
            ================ Certificate 5 ================\n\
            Serial Number: 00abcdef1234567890\n\
            Issuer: CN=rcgen self signed cert\n";
        assert!(store_contains(dump, der));
        assert!(!store_contains("no certificates here", der));
    }

    #[test]
    fn sign_padding_zeros_do_not_hide_a_serial() {
        assert!(store_dump_contains("Seriennummer: 00abc123", "00abc123"));
        assert!(store_dump_contains("Serial Number: abc123", "00abc123"));
        assert!(!store_dump_contains("Serial Number: 000000", "00abc123"));
    }
}
