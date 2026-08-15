#!/bin/bash
# Build and install g3 and studio to ~/.local/bin

set -e

cd "$(dirname "$0")/.."

# --allow-adhoc: knowingly permit ad-hoc signing. Ad-hoc binaries run fine but
# their designated requirement is a hash of the bytes, so macOS treats every
# rebuild as a new program and all TCC permission grants (Full Disk Access etc.)
# are dropped and re-prompted. Never the default; must be asked for explicitly.
ALLOW_ADHOC="no"
# Seconds to wait for keychain/Touch ID when signing is bounced via Terminal.app.
SIGN_TIMEOUT="${G3_SIGN_TIMEOUT:-120}"

for arg in "$@"; do
    case "$arg" in
        --allow-adhoc) ALLOW_ADHOC="yes" ;;
        -h|--help)
            echo "Usage: $0 [--allow-adhoc]"
            echo ""
            echo "  --allow-adhoc   Permit ad-hoc signing if cert signing fails."
            echo "                  WARNING: kills all macOS TCC grants and makes"
            echo "                  them re-prompt after every rebuild."
            echo ""
            echo "Env: G3_SIGN_TIMEOUT=<seconds>  (default 120)"
            exit 0
            ;;
        *) echo "Unknown argument: $arg (try --help)" >&2; exit 2 ;;
    esac
done

# Overridable so tests can exercise the signing logic without clobbering the
# live binary. Defaults to the real install location.
INSTALL_DIR="${G3_INSTALL_DIR:-$HOME/.local/bin}"
# Tests set this to skip the (slow) cargo build and install placeholder files.
SKIP_BUILD="${G3_SKIP_BUILD:-no}"
mkdir -p "$INSTALL_DIR"

if [[ "$SKIP_BUILD" == "yes" ]]; then
    # Test mode: fabricate installable binaries without a Rust build. Uses a real
    # signable Mach-O (this shell) so codesign behaves authentically.
    echo "Building g3 and studio (release)... [SKIPPED: G3_SKIP_BUILD=yes]"
    echo "Installing to $INSTALL_DIR..."
    cp /bin/bash "$INSTALL_DIR/g3"
    cp /bin/bash "$INSTALL_DIR/g3-studio"
    chmod +w "$INSTALL_DIR/g3" "$INSTALL_DIR/g3-studio"
else
    echo "Building g3 and studio (release)..."
    cargo build --release -p g3 -p studio

    echo "Installing to $INSTALL_DIR..."
    cp target/release/g3 "$INSTALL_DIR/"
    cp target/release/studio "$INSTALL_DIR/g3-studio"
fi

# ---------------------------------------------------------------------------
# Re-sign binaries after copying.
#
# CRITICAL: signing must use the "Butler Local Signing" CERTIFICATE, not ad-hoc.
# macOS TCC stores the binary's designated requirement (DR) alongside every
# permission grant (Full Disk Access, App Management, Automation...):
#
#   cert-signed  DR: identifier "com.wideplay.butler.g3" and certificate leaf = H"1359..."
#   ad-hoc       DR: cdhash H"<hash of the binary bytes>"
#
# The cert DR names an IDENTITY, so grants survive every rebuild. An ad-hoc DR is
# a content hash, so each rebuild looks like a brand new program to macOS and all
# TCC grants silently die -- which is why "g3 would like to access data from other
# apps" used to reappear after every single rebuild.
#
# This script therefore NEVER falls back to ad-hoc signing implicitly. Pass
# --allow-adhoc if you knowingly want a throwaway build with dead permissions.
# ---------------------------------------------------------------------------
if [[ "$OSTYPE" == "darwin"* ]]; then
    SIGN_IDENTITY="Butler Local Signing"
    CHECK_SIGNING="$(dirname "$0")/check-signing.sh"

    # Sign both binaries. Returns non-zero if either codesign call fails.
    sign_binaries() {
        codesign --force --sign "$SIGN_IDENTITY" \
            --identifier "com.wideplay.butler.g3" "$INSTALL_DIR/g3" &&
        codesign --force --sign "$SIGN_IDENTITY" \
            --identifier "com.wideplay.butler.g3-studio" "$INSTALL_DIR/g3-studio"
    }

    adhoc_sign() {
        codesign --force --sign - "$INSTALL_DIR/g3"
        codesign --force --sign - "$INSTALL_DIR/g3-studio"
    }

    # Abort loudly rather than leave a permission-poisoning binary installed.
    # $1 = human explanation of what went wrong.
    signing_failed() {
        if [[ "$ALLOW_ADHOC" == "yes" ]]; then
            echo ""
            echo "   WARNING: $1"
            echo "   WARNING: --allow-adhoc given: signing ad-hoc anyway."
            echo "       This binary will run, but EVERY macOS permission grant"
            echo "       (Full Disk Access, App Management, Automation) will be"
            echo "       dropped now and re-prompted after every future rebuild."
            adhoc_sign
            return 0
        fi
        echo "" >&2
        echo "Install aborted: $1" >&2
        echo "" >&2
        echo "   Refusing to ad-hoc sign. An ad-hoc binary runs fine but destroys" >&2
        echo "   every macOS TCC grant, and re-prompts on every rebuild forever." >&2
        echo "" >&2
        echo "   Fix the signing identity, then re-run. Options:" >&2
        echo "     - unlock your login keychain (Keychain Access) and retry" >&2
        echo "     - run this script from a normal Terminal window (Aqua session)" >&2
        echo "     - verify the cert exists: security find-identity -v -p codesigning" >&2
        echo "     - knowingly accept broken permissions: $0 --allow-adhoc" >&2
        echo "" >&2
        echo "   NOTE: $INSTALL_DIR/g3 was already overwritten by this run, so it" >&2
        echo "   is now stale/unsigned. Re-run once signing works." >&2
        exit 1
    }

    if security find-identity -v -p codesigning 2>&1 | grep -q "$SIGN_IDENTITY"; then
        echo "Re-signing binaries for macOS (identity: $SIGN_IDENTITY)..."

        # codesign needs the login keychain, which needs the Security agent,
        # which is only reachable from an Aqua (GUI) session. A build launched
        # from a LaunchAgent/daemon context reports `launchctl managername` =
        # Background and codesign dies with errSecInternalComponent. Bounce the
        # signing step through Terminal.app so it lands in the Aqua session.
        if ! sign_binaries; then
            if [[ "$(launchctl managername 2>/dev/null)" != "Aqua" ]] \
               && command -v osascript >/dev/null 2>&1; then
                echo "   Signing failed in a $(launchctl managername 2>/dev/null) session;"
                echo "   retrying via Terminal.app (Aqua) so the keychain is reachable..."

                SIGN_LOG="$(mktemp -t g3sign)"
                SIGN_SCRIPT="$(mktemp -t g3sign_sh)"
                cat > "$SIGN_SCRIPT" <<SIGNEOF
#!/bin/bash
codesign --force --sign "$SIGN_IDENTITY" \
    --identifier "com.wideplay.butler.g3" "$INSTALL_DIR/g3" > "$SIGN_LOG" 2>&1
g3_rc=\$?
codesign --force --sign "$SIGN_IDENTITY" \
    --identifier "com.wideplay.butler.g3-studio" "$INSTALL_DIR/g3-studio" >> "$SIGN_LOG" 2>&1
studio_rc=\$?
echo "rc=\$((g3_rc + studio_rc))" >> "$SIGN_LOG"
SIGNEOF
                chmod +x "$SIGN_SCRIPT"
                osascript -e "tell application \"Terminal\" to do script \"$SIGN_SCRIPT; exit\"" \
                    >/dev/null 2>&1 || true

                # Wait for the Aqua-side signing to land. This may need human
                # attention (Touch ID / keychain unlock), so allow a generous
                # window -- the old 30s timeout used to expire while Dhanji was
                # away from the machine, silently producing an ad-hoc install.
                echo "   (waiting up to ${SIGN_TIMEOUT}s -- approve any keychain/Touch ID prompt)"
                for i in $(seq 1 "$SIGN_TIMEOUT"); do
                    grep -q '^rc=' "$SIGN_LOG" 2>/dev/null && break
                    # Nudge at the halfway mark so a waiting prompt gets noticed.
                    if [[ $i -eq $((SIGN_TIMEOUT / 2)) ]]; then
                        echo "   still waiting -- check for a keychain prompt in Terminal.app"
                    fi
                    sleep 1
                done
                rm -f "$SIGN_SCRIPT"

                if grep -q '^rc=0$' "$SIGN_LOG" 2>/dev/null; then
                    echo "   Signed via Aqua session"
                    rm -f "$SIGN_LOG"
                elif grep -q '^rc=' "$SIGN_LOG" 2>/dev/null; then
                    # Genuine codesign failure: it ran and returned non-zero.
                    echo "   codesign output:" >&2
                    sed 's/^/      /' "$SIGN_LOG" >&2 2>/dev/null
                    rm -f "$SIGN_LOG"
                    signing_failed "codesign failed in the Aqua session (see output above)."
                else
                    # No rc= line at all: it never completed. Distinct from the
                    # above -- almost always a keychain prompt nobody answered.
                    echo "   codesign output (incomplete):" >&2
                    sed 's/^/      /' "$SIGN_LOG" >&2 2>/dev/null
                    rm -f "$SIGN_LOG"
                    signing_failed "Aqua signing did not complete within ${SIGN_TIMEOUT}s -- an unanswered keychain/Touch ID prompt is the usual cause. Unlock your login keychain and re-run."
                fi
            else
                signing_failed "codesign failed in an Aqua session -- your login keychain is probably locked."
            fi
        fi
    else
        signing_failed "signing identity '$SIGN_IDENTITY' not found in the keychain."
    fi

    # ---- Assert the result is DURABLY signed. -------------------------------
    # `codesign --verify --strict` is NOT sufficient: an ad-hoc signature is
    # internally coherent and passes it cleanly, even when given the correct
    # --identifier. Only checking the designated requirement (identifier AND
    # certificate leaf) catches that. Delegated to check-signing.sh, which is
    # mutation-tested by butler's tools/tests/test_g3_signing.sh.
    if [[ -x "$CHECK_SIGNING" ]]; then
        if ! "$CHECK_SIGNING" "$INSTALL_DIR/g3" "com.wideplay.butler.g3"; then
            if [[ "$ALLOW_ADHOC" == "yes" ]]; then
                echo "   WARNING: proceeding anyway because --allow-adhoc was given."
            else
                echo "" >&2
                echo "Install aborted: $INSTALL_DIR/g3 is not durably signed." >&2
                exit 1
            fi
        fi
    else
        # Fallback if the check script is missing: still refuse a binary whose
        # identifier is wrong (weaker -- does not verify the cert leaf).
        if ! codesign --verify --strict "$INSTALL_DIR/g3" 2>/dev/null; then
            echo "$INSTALL_DIR/g3 failed signature verification" >&2
            exit 1
        fi
        ACTUAL_ID="$(codesign -dv "$INSTALL_DIR/g3" 2>&1 | sed -n 's/^Identifier=//p')"
        if [[ "$ACTUAL_ID" != "com.wideplay.butler.g3" && "$ALLOW_ADHOC" != "yes" ]]; then
            echo "Wrong signing identifier: ${ACTUAL_ID:-<none>}" >&2
            echo "   (check-signing.sh missing, so the cert leaf was not verified)" >&2
            exit 1
        fi
        echo "   Signature identifier: ${ACTUAL_ID:-unknown}"
    fi
fi

# Create symlink to override Android Studio's 'studio' command
# Remove existing symlink if present, but don't remove if it's a different file
if [ -L "$INSTALL_DIR/studio" ]; then
    rm "$INSTALL_DIR/studio"
fi
ln -s "$INSTALL_DIR/g3-studio" "$INSTALL_DIR/studio"

echo "Done! Installed:"
echo "  $INSTALL_DIR/g3"
echo "  $INSTALL_DIR/g3-studio"
echo "  $INSTALL_DIR/studio -> g3-studio"

# Check if ~/.local/bin is in PATH and fix if needed
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    echo ""
    echo "⚠️  $INSTALL_DIR is not in your PATH"
    
    # Detect shell config file
    SHELL_NAME=$(basename "$SHELL")
    case "$SHELL_NAME" in
        zsh)  RC_FILE="$HOME/.zshrc" ;;
        bash) 
            # macOS uses .bash_profile, Linux uses .bashrc
            if [[ "$OSTYPE" == "darwin"* ]]; then
                RC_FILE="$HOME/.bash_profile"
            else
                RC_FILE="$HOME/.bashrc"
            fi
            ;;
        fish) RC_FILE="$HOME/.config/fish/config.fish" ;;
        *)    RC_FILE="" ;;
    esac
    
    if [ -n "$RC_FILE" ]; then
        # Check if it's already in the rc file (just not loaded in current session)
        if grep -q '\.local/bin' "$RC_FILE" 2>/dev/null; then
            echo "   (Already in $RC_FILE, just not loaded in this session)"
            echo "   Run: source $RC_FILE"
        else
            echo ""
            read -p "   Add to $RC_FILE? [Y/n] " -n 1 -r
            echo
            if [[ $REPLY =~ ^[Yy]$ ]] || [[ -z $REPLY ]]; then
                echo '' >> "$RC_FILE"
                if [[ "$SHELL_NAME" == "fish" ]]; then
                    echo 'set -gx PATH $HOME/.local/bin $PATH' >> "$RC_FILE"
                else
                    echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$RC_FILE"
                fi
                echo "   ✅ Added to $RC_FILE"
                echo "   Run: source $RC_FILE"
            else
                echo "   Skipped. Add manually:"
                echo "   export PATH=\"\$HOME/.local/bin:\$PATH\""
            fi
        fi
    else
        echo "   Unknown shell ($SHELL_NAME). Add this to your shell rc file:"
        echo "   export PATH=\"\$HOME/.local/bin:\$PATH\""
    fi
fi
