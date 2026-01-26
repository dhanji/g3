#!/bin/bash
# Setup Chrome for Testing with matching ChromeDriver
# This ensures version compatibility for WebDriver automation

set -e

# Configuration
INSTALL_DIR="${HOME}/.chrome-for-testing"
BIN_DIR="${HOME}/.local/bin"

# Detect OS and architecture
OS=$(uname -s)
ARCH=$(uname -m)

if [ "$OS" = "Darwin" ]; then
    if [ "$ARCH" = "arm64" ]; then
        PLATFORM="mac-arm64"
    elif [ "$ARCH" = "x86_64" ]; then
        PLATFORM="mac-x64"
    else
        echo "Unsupported architecture: $ARCH"
        exit 1
    fi
elif [ "$OS" = "Linux" ]; then
    if [ "$ARCH" = "x86_64" ]; then
        PLATFORM="linux64"
    elif [ "$ARCH" = "aarch64" ]; then
        PLATFORM="linux-arm64"
    else
        echo "Unsupported architecture: $ARCH"
        exit 1
    fi
else
    echo "Unsupported operating system: $OS"
    exit 1
fi

echo "Detected platform: $OS/$ARCH -> $PLATFORM"

# Get latest stable version info
echo "📡 Fetching latest Chrome for Testing version..."
VERSION_JSON=$(curl -s 'https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json')
VERSION=$(echo "$VERSION_JSON" | python3 -c "import json,sys; print(json.load(sys.stdin)['channels']['Stable']['version'])")

echo "📦 Latest stable version: $VERSION"

# Get download URLs
CHROME_URL=$(echo "$VERSION_JSON" | python3 -c "
import json,sys
data = json.load(sys.stdin)
for d in data['channels']['Stable']['downloads']['chrome']:
    if d['platform'] == '$PLATFORM':
        print(d['url'])
        break
")

CHROMEDRIVER_URL=$(echo "$VERSION_JSON" | python3 -c "
import json,sys
data = json.load(sys.stdin)
for d in data['channels']['Stable']['downloads']['chromedriver']:
    if d['platform'] == '$PLATFORM':
        print(d['url'])
        break
")

# Create directories
mkdir -p "$INSTALL_DIR"
mkdir -p "$BIN_DIR"

# Download and extract Chrome for Testing
echo "⬇️  Downloading Chrome for Testing..."
cd "$INSTALL_DIR"
curl -L -o chrome.zip "$CHROME_URL"
unzip -q -o chrome.zip
rm chrome.zip

# The extracted folder name varies by platform
CHROME_APP_DIR="chrome-$PLATFORM"
if [ -d "$CHROME_APP_DIR" ]; then
    echo "✅ Chrome for Testing installed to: $INSTALL_DIR/$CHROME_APP_DIR"
else
    echo "❌ Chrome extraction failed"
    exit 1
fi

# Download and extract ChromeDriver
echo "⬇️  Downloading ChromeDriver..."
curl -L -o chromedriver.zip "$CHROMEDRIVER_URL"
unzip -q -o chromedriver.zip
rm chromedriver.zip

CHROMEDRIVER_DIR="chromedriver-$PLATFORM"
if [ -f "$CHROMEDRIVER_DIR/chromedriver" ]; then
    # Create symlinks in bin directory
    # Primary symlink: 'chromedriver' - works with g3 out of the box
    ln -sf "$INSTALL_DIR/$CHROMEDRIVER_DIR/chromedriver" "$BIN_DIR/chromedriver"
    # Secondary symlink: 'chromedriver-for-testing' - explicit name to avoid confusion
    ln -sf "$INSTALL_DIR/$CHROMEDRIVER_DIR/chromedriver" "$BIN_DIR/chromedriver-for-testing"
    chmod +x "$INSTALL_DIR/$CHROMEDRIVER_DIR/chromedriver"
    echo "✅ ChromeDriver installed and linked to: $BIN_DIR/chromedriver"
else
    echo "❌ ChromeDriver extraction failed"
    exit 1
fi

# Create a wrapper script that uses Chrome for Testing
cat > "$BIN_DIR/chrome-for-testing" << 'EOF'
#!/bin/bash
INSTALL_DIR="${HOME}/.chrome-for-testing"
OS=$(uname -s)
ARCH=$(uname -m)

if [ "$OS" = "Darwin" ]; then
    if [ "$ARCH" = "arm64" ]; then
        PLATFORM="mac-arm64"
    else
        PLATFORM="mac-x64"
    fi
    exec "$INSTALL_DIR/chrome-$PLATFORM/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing" "$@"
elif [ "$OS" = "Linux" ]; then
    if [ "$ARCH" = "x86_64" ]; then
        PLATFORM="linux64"
    else
        PLATFORM="linux-arm64"
    fi
    exec "$INSTALL_DIR/chrome-$PLATFORM/chrome" "$@"
else
    echo "Unsupported operating system: $OS"
    exit 1
fi
EOF
chmod +x "$BIN_DIR/chrome-for-testing"

echo ""
echo "✅ Setup complete!"
echo ""
echo "Installed versions:"
echo "  Chrome for Testing: $VERSION"
echo "  ChromeDriver: $VERSION"
echo ""
echo "Binaries:"
echo "  Chrome: $BIN_DIR/chrome-for-testing"
echo "  ChromeDriver: $BIN_DIR/chromedriver"
echo ""
echo "To use with g3, make sure $BIN_DIR is in your PATH:"
echo "  export PATH=\"$BIN_DIR:\$PATH\""
echo ""
echo "Or add to your shell profile (~/.zshrc or ~/.bashrc):"
echo "  echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.zshrc"
