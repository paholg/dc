//! End-to-end check of the Root store against the real certutil.exe.
//!
//! CI's windows-store-e2e job runs this on native Windows: hosted runners
//! can't run WSL2, so this exercises what `dc proxy trust` drives from WSL.
//! Mutates the machine Root store, under a test-only CN so even a failed
//! run can't strand or clobber a real installation.

use std::path::Path;

use eyre::{Result, bail, ensure};

// Never the production CN.
const CN: &str = "devconcurrent winstore e2e CA";

// The CLI uses the user store, but its addstore hangs headless on the
// confirmation dialog; the machine store trades the dialog for admin, which
// a runner has, and shares every other certutil semantic under test.
const SCOPE: winstore::Scope = winstore::Scope::Machine;

fn root(serial: &[u8]) -> rcgen::Certificate {
    let mut params = rcgen::CertificateParams::default();
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String(CN.to_string()),
    );
    params.serial_number = Some(rcgen::SerialNumber::from_slice(serial));
    params
        .self_signed(&rcgen::KeyPair::generate().unwrap())
        .unwrap()
}

fn main() -> Result<()> {
    if !cfg!(windows) {
        bail!("this end-to-end check drives certutil.exe; run it on Windows");
    }
    let certutil = Path::new("certutil.exe");

    // A leftover from an earlier failed run would break the absence checks.
    winstore::uninstall(certutil, SCOPE, CN)?;

    // Two roots under one CN, like a regenerated root re-trusted without
    // untrusting. The first serial's high bit is set, so its DER form
    // carries a sign-padding zero; the second's is clear.
    let with_padding = root(&[0xAB, 0xCD, 0xEF, 0x12, 0x34, 0x56, 0x78, 0x90]);
    let without = root(&[0x7B, 0xCD, 0xEF, 0x12, 0x34, 0x56, 0x78, 0x90]);

    let dump = winstore::root_store(certutil, SCOPE, CN)?;
    ensure!(dump.is_empty(), "the store should start empty: {dump}");

    let dir = std::env::temp_dir().join(format!("winstore-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    for (name, cert) in [("with.pem", &with_padding), ("without.pem", &without)] {
        let path = dir.join(name);
        std::fs::write(&path, cert.pem())?;
        // Headless, this must succeed without raising the confirmation
        // dialog (which would hang until the job times out).
        winstore::install(certutil, SCOPE, path.to_str().unwrap())?;
    }
    println!("addstore: OK");

    let dump = winstore::root_store(certutil, SCOPE, CN)?;
    ensure!(
        winstore::store_contains(&dump, with_padding.der()),
        "the sign-padded serial wasn't found:\n{dump}",
    );
    ensure!(
        winstore::store_contains(&dump, without.der()),
        "the unpadded serial wasn't found:\n{dump}",
    );
    // A root that was never installed isn't found.
    let other = root(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    ensure!(
        !winstore::store_contains(&dump, other.der()),
        "an uninstalled root was found:\n{dump}",
    );
    println!("store dump: OK");

    // Whether delstore removes one match per call or all of them, the loop
    // must clear the store and terminate.
    let removed = winstore::uninstall(certutil, SCOPE, CN)?;
    ensure!(
        (1..=2).contains(&removed),
        "{removed} delstore passes for 2 certs"
    );
    ensure!(
        winstore::root_store(certutil, SCOPE, CN)?.is_empty(),
        "still in the store after uninstall",
    );
    println!("delstore: OK ({removed} pass(es))");

    let _ = std::fs::remove_dir_all(&dir);
    println!("ALL OK");
    Ok(())
}
