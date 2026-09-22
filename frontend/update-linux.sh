#!/bin/bash
# Update an existing Linux AppImage install of Meetily.
# Pulls latest source, rebuilds with build-gpu.sh, and atomically replaces
# the installed AppImage (safe even while the old one is still running).

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}🔄 Meetily Linux Updater${NC}"
echo ""

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Where the AppImage gets installed. Override with INSTALL_DIR/INSTALL_NAME
# if you keep it somewhere other than ~/Applications.
INSTALL_DIR="${INSTALL_DIR:-$HOME/Applications}"
INSTALL_NAME="${INSTALL_NAME:-Meetily-ActuallyFree.AppImage}"
INSTALL_PATH="$INSTALL_DIR/$INSTALL_NAME"

# Pull latest source
if [ -d "$REPO_ROOT/.git" ]; then
    if [ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]; then
        echo -e "${RED}❌ Uncommitted changes in $REPO_ROOT${NC}"
        echo -e "${RED}   Commit, stash, or discard them before updating.${NC}"
        exit 1
    fi
    echo -e "${BLUE}📥 Pulling latest source...${NC}"
    git -C "$REPO_ROOT" pull
else
    echo -e "${YELLOW}⚠️ $REPO_ROOT is not a git checkout - skipping git pull${NC}"
fi

# Install JS dependencies (picks up any dependency changes from the pull)
cd "$SCRIPT_DIR"
if command_exists pnpm; then
    pnpm install
elif command_exists npm; then
    npm install
else
    echo -e "${RED}❌ Neither npm nor pnpm found${NC}"
    exit 1
fi

# Rebuild (respects TAURI_GPU_FEATURE if already exported; auto-detects otherwise)
echo ""
echo -e "${BLUE}🔨 Rebuilding...${NC}"
bash "$SCRIPT_DIR/build-gpu.sh"

# Find the freshly built AppImage
BUNDLE_DIR="$REPO_ROOT/target/release/bundle/appimage"
NEW_APPIMAGE=$(find "$BUNDLE_DIR" -maxdepth 1 -name "*.AppImage" -print0 2>/dev/null | xargs -0 ls -t 2>/dev/null | head -n 1)

if [ -z "$NEW_APPIMAGE" ] || [ ! -f "$NEW_APPIMAGE" ]; then
    echo -e "${RED}❌ Could not find a built AppImage in $BUNDLE_DIR${NC}"
    exit 1
fi

# Replace the installed copy atomically (safe even if the old one is
# currently running - mv is a rename on the same filesystem, so an already
# running process keeps its open file handle to the old inode).
mkdir -p "$INSTALL_DIR"
TMP_PATH="$INSTALL_PATH.new"
cp "$NEW_APPIMAGE" "$TMP_PATH"
chmod +x "$TMP_PATH"
mv -f "$TMP_PATH" "$INSTALL_PATH"

echo ""
echo -e "${GREEN}✅ Updated $INSTALL_PATH${NC}"
echo -e "${YELLOW}   Restart Meetily if it's currently running.${NC}"
