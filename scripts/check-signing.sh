#!/bin/bash
# Verify a binary is signed with the Butler Local Signing identity in a way that
# SURVIVES REBUILDS, i.e. its designated requirement names an *identity*
# (identifier + certificate leaf) rather than a content hash.
#
# WHY THIS EXISTS
# ---------------
# macOS TCC stores the binary's designated requirement (DR) alongside each
# permission grant (Full Disk Access, App Management, Automation...).
#
#   cert-signed  DR: identifier "com.wideplay.butler.g3" and certificate leaf = H"1359..."
#   ad-hoc       DR: cdhash H"4f150fb1..."
#
# The cert DR names an identity, so a rebuild still matches and the grant holds
# forever. The ad-hoc DR is a hash OF THE BINARY BYTES, so every rebuild looks
# like a brand new stranger to macOS and every TCC grant silently dies -- which
# is why "g3 would like to access data from other apps" used to come back after
# every single rebuild.
#
# `codesign --verify --strict` does NOT catch this: an ad-hoc signature is
# internally coherent and passes cleanly. Only checking the DR catches it.
#
# Usage: check-signing.sh [/path/to/binary] [expected-identifier]
# Exits 0 if the binary is cert-signed with a rebuild-durable DR, non-zero otherwise.

set -u

SIGN_IDENTITY="Butler Local Signing"
BINARY="${1:-$HOME/.local/bin/g3}"
EXPECTED_ID="${2:-com.wideplay.butler.g3}"

fail() {
    echo "❌ $1" >&2
    return 1
}

# --- The binary must actually exist. A missing file must FAIL, never pass
# --- vacuously -- otherwise a broken install reports success.
if [[ ! -f "$BINARY" ]]; then
    fail "No such binary: $BINARY"
    exit 1
fi

# --- Resolve the signing certificate's SHA-1 fingerprint from the keychain.
# --- This is what the DR must pin, and it is what makes the grant durable.
FINGERPRINT="$(security find-certificate -c "$SIGN_IDENTITY" -p 2>/dev/null \
    | openssl x509 -noout -fingerprint -sha1 2>/dev/null \
    | sed 's/.*=//; s/://g' | tr 'A-Z' 'a-z')"

if [[ -z "$FINGERPRINT" ]]; then
    fail "Signing certificate '$SIGN_IDENTITY' not found in keychain.
   Without it every install is ad-hoc and TCC grants die on every rebuild.
   See butler docs (skills/duty-system) for cert setup."
    exit 1
fi

# --- The authoritative test. -R makes codesign evaluate the binary against a
# --- requirement of our choosing, rather than its own self-declared DR (which
# --- is exactly what an ad-hoc binary would happily satisfy).
REQUIREMENT="identifier \"$EXPECTED_ID\" and certificate leaf = H\"$FINGERPRINT\""

if ! VERIFY_OUT="$(codesign --verify --strict -R="$REQUIREMENT" "$BINARY" 2>&1)"; then
    ACTUAL_ID="$(codesign -dv "$BINARY" 2>&1 | sed -n 's/^Identifier=//p')"
    ACTUAL_DR="$(codesign -d -r- "$BINARY" 2>&1 | sed -n 's/^designated => //p;s/^# designated => //p')"
    fail "$BINARY is NOT durably signed with '$SIGN_IDENTITY'.

   expected identifier : $EXPECTED_ID
   actual identifier   : ${ACTUAL_ID:-<none>}
   actual requirement  : ${ACTUAL_DR:-<none>}
   codesign said       : $VERIFY_OUT

   If the actual requirement is a bare 'cdhash H\"...\"' this binary is AD-HOC
   signed. It will run fine, but macOS will treat every future rebuild as a new
   program and re-prompt for (or silently drop) every TCC permission."
    exit 1
fi

echo "✅ $BINARY"
echo "   identifier:  $EXPECTED_ID"
echo "   cert leaf:   $FINGERPRINT"
echo "   TCC grants (Full Disk Access etc.) will survive rebuilds."
exit 0
