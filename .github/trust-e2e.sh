#!/usr/bin/env bash
set -euo pipefail

# End-to-end test for `dc proxy trust` / `dc proxy untrust`: install the CA,
# verify it landed in the system and NSS stores, remove it, verify it's gone.
#
# Expects either passwordless sudo (a GitHub runner) or to be run as root (a
# distro container; `dc` skips sudo then). Mutates the machine's trust
# stores — don't run it anywhere you care about.

dc=${DC:-target/debug/devconcurrent}
fail() { echo "FAIL: $*" >&2; exit 1; }
maybe_sudo() { if [ "$(id -u)" = 0 ]; then "$@"; else sudo "$@"; fi }

os=$(uname -s)

# Minimal config; `dc` requires the file to exist.
if [ "$os" = Darwin ]; then
  config_dir="$HOME/Library/Application Support/devconcurrent"
else
  config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/devconcurrent"
fi
mkdir -p "$config_dir" && touch "$config_dir/config.toml"

# A fake Firefox profile exercises the NSS path.
if [ "$os" = Darwin ]; then
  profile="$HOME/Library/Application Support/Firefox/Profiles/e2e.default"
  certutil="$(brew --prefix nss)/bin/certutil"
else
  profile="$HOME/.mozilla/firefox/e2e.default"
  certutil=certutil
fi
mkdir -p "$profile"
"$certutil" -N -d "sql:$profile" --empty-password

echo "=== trust ==="
"$dc" proxy trust
ca="$("$dc" show ca-root)/rootCA.pem"

if [ "$os" = Darwin ]; then
  security find-certificate -c "devconcurrent root CA" \
    /Library/Keychains/System.keychain >/dev/null \
    || fail "root not in the system keychain"
  security verify-cert -c "$ca" >/dev/null \
    || fail "the system keychain does not trust the root"
else
  for bundle in /etc/ssl/certs/ca-certificates.crt \
                /etc/pki/tls/certs/ca-bundle.crt \
                /etc/ca-certificates/extracted/tls-ca-bundle.pem; do
    if [ -e "$bundle" ]; then break; fi
  done
  [ -e "$bundle" ] || fail "no known CA bundle on this machine"
  openssl verify -CAfile "$bundle" "$ca" >/dev/null || fail "root not in $bundle"

  # `|| true`: find fails on the anchor dirs the other distros use.
  anchor=$(find /usr/local/share/ca-certificates \
                /etc/pki/ca-trust/source/anchors \
                /etc/ca-certificates/trust-source/anchors \
                -maxdepth 1 -name 'devconcurrent_*' 2>/dev/null | head -1 || true)
  [ -n "$anchor" ] || fail "no anchor file installed"
  # A leftover from the old `CAROOT=... mkcert -install` instructions;
  # untrust should clean it up too.
  legacy=$(dirname "$anchor")/$(basename "$anchor" \
    | sed s/devconcurrent_development/mkcert_development/)
  maybe_sudo cp "$anchor" "$legacy"
fi
echo "system store: OK"

"$certutil" -L -d "sql:$profile" | grep -q "devconcurrent development CA" \
  || fail "root not in the NSS profile"
echo "nss: OK"

echo "=== untrust ==="
# untrust deletes the CA files at the end; keep a copy for the store checks.
ca_copy=$(mktemp)
cp "$ca" "$ca_copy"
"$dc" proxy untrust

[ ! -e "$ca" ] || fail "rootCA.pem still present"
[ ! -e "$(dirname "$ca")/rootCA-key.pem" ] || fail "rootCA-key.pem still present"

if [ "$os" = Darwin ]; then
  if security verify-cert -c "$ca_copy" >/dev/null 2>&1; then
    fail "the system keychain still trusts the root"
  fi
else
  if openssl verify -CAfile "$bundle" "$ca_copy" >/dev/null 2>&1; then
    fail "root still in $bundle"
  fi
  [ ! -e "$anchor" ] || fail "anchor still present"
  [ ! -e "$legacy" ] || fail "legacy mkcert anchor still present"
fi
if "$certutil" -L -d "sql:$profile" | grep -q "devconcurrent development CA"; then
  fail "root still in the NSS profile"
fi

echo "ALL OK"
