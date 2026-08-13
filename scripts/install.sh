#!/bin/bash
# Build and install g3 and studio to ~/.local/bin

set -e

cd "$(dirname "$0")/.."

INSTALL_DIR="$HOME/.local/bin"
mkdir -p "$INSTALL_DIR"

echo "Building g3 and studio (release)..."
cargo build --release -p g3 -p studio

echo "Installing to $INSTALL_DIR..."
cp target/release/g3 "$INSTALL_DIR/"
cp target/release/studio "$INSTALL_DIR/g3-studio"

# Re-sign binaries after copying (required on macOS to avoid security policy rejection)
if [[ "$OSTYPE" == "darwin"* ]]; then
    SIGN_IDENTITY="Butler Local Signing"

    # Sign both binaries. Returns non-zero if either codesign call fails.
    sign_binaries() {
        codesign --force --sign "$SIGN_IDENTITY" \
            --identifier "com.wideplay.butler.g3" "$INSTALL_DIR/g3" &&
        codesign --force --sign "$SIGN_IDENTITY" \
            --identifier "com.wideplay.butler.g3-studio" "$INSTALL_DIR/g3-studio"
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
                echo "   ↻ Signing failed in a $(launchctl managername 2>/dev/null) session;"
                echo "     retrying via Terminal.app (Aqua) so the keychain is reachable..."

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

                # Wait for the Aqua-side signing to land (Touch ID / keychain
                # prompt may need a moment of human attention).
                for _ in $(seq 1 30); do
                    grep -q '^rc=' "$SIGN_LOG" 2>/dev/null && break
                    sleep 1
                done
                rm -f "$SIGN_SCRIPT"

                if grep -q '^rc=0$' "$SIGN_LOG" 2>/dev/null; then
                    echo "   ✅ Signed via Aqua session"
                else
                    echo "   ⚠️  Aqua signing did not confirm. codesign output:"
                    sed 's/^/      /' "$SIGN_LOG" 2>/dev/null
                    echo "      Falling back to ad-hoc signing."
                    codesign --force --sign - "$INSTALL_DIR/g3"
                    codesign --force --sign - "$INSTALL_DIR/g3-studio"
                fi
                rm -f "$SIGN_LOG"
            else
                echo "   ⚠️  Signing failed in an Aqua session — unlock the login keychain."
                echo "      Falling back to ad-hoc signing."
                codesign --force --sign - "$INSTALL_DIR/g3"
                codesign --force --sign - "$INSTALL_DIR/g3-studio"
            fi
        fi
    else
        echo "⚠️  Signing identity '$SIGN_IDENTITY' not found — falling back to ad-hoc signing"
        echo "   (You may get repeated macOS permission prompts. See butler docs for cert setup.)"
        codesign --force --sign - "$INSTALL_DIR/g3"
        codesign --force --sign - "$INSTALL_DIR/g3-studio"
    fi

    # Never leave a silently-unsigned binary behind.
    if ! codesign --verify --strict "$INSTALL_DIR/g3" 2>/dev/null; then
        echo "❌ $INSTALL_DIR/g3 failed signature verification" >&2
        exit 1
    fi
    ACTUAL_ID="$(codesign -dv "$INSTALL_DIR/g3" 2>&1 | sed -n 's/^Identifier=//p')"
    echo "   Signature identifier: ${ACTUAL_ID:-unknown}"
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
