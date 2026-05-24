#!/usr/bin/env bash
# Signs Tauri macOS bundles (.app.tar.gz) and emits/updates latest.json.
#
# Usage:
#   ./publish-update.sh business 1.0.1 "Bug fixes."
#
# Env vars:
#   TAURI_SIGNING_PRIVATE_KEY          - path to the private key (same one as Windows)
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD - key password
#
# Run AFTER `pnpm desktop:build:business` (or :admin) on a macOS machine.
# This script does NOT upload anything — copy the contents of
# desktop/dist/<app>/ to your CDN at https://orcaa.cloud/desktop/<app>/

set -euo pipefail

APP="${1:-}"
VERSION="${2:-}"
NOTES="${3:-Improvements and fixes.}"
BASE_URL="${BASE_URL:-https://github.com/LogixOrg/orcaa-desktop/releases/download/v${VERSION}}"

if [[ -z "$APP" || -z "$VERSION" ]]; then
  echo "Usage: $0 <business|admin> <version> [notes]" >&2
  exit 1
fi

if [[ "$APP" != "business" && "$APP" != "admin" ]]; then
  echo "App must be 'business' or 'admin'." >&2
  exit 1
fi

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
  echo "TAURI_SIGNING_PRIVATE_KEY env var required (path to private key)." >&2
  exit 1
fi

if [[ ! -f "$TAURI_SIGNING_PRIVATE_KEY" ]]; then
  echo "Private key not found: $TAURI_SIGNING_PRIVATE_KEY" >&2
  exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUNDLE_DIR="$REPO_ROOT/src-tauri/target/release/bundle/macos"
DMG_DIR="$REPO_ROOT/src-tauri/target/release/bundle/dmg"
OUT_DIR="$REPO_ROOT/dist/$APP"

if [[ ! -d "$BUNDLE_DIR" ]]; then
  echo "Bundle dir missing: $BUNDLE_DIR" >&2
  echo "Run 'pnpm build:$APP' first." >&2
  exit 1
fi

# Detect arch from host (CI matrix usually overrides this)
ARCH="${ARCH:-$(uname -m)}"
case "$ARCH" in
  arm64|aarch64) PLATFORM_KEY="darwin-aarch64" ;;
  x86_64)        PLATFORM_KEY="darwin-x86_64" ;;
  *) echo "Unknown arch: $ARCH" >&2; exit 1 ;;
esac

TARBALL=$(find "$BUNDLE_DIR" -maxdepth 1 -name "*.app.tar.gz" -print -quit)
if [[ -z "$TARBALL" ]]; then
  echo "No .app.tar.gz found in $BUNDLE_DIR — make sure 'app' is in bundle.targets and the build ran." >&2
  exit 1
fi

echo "Signing $(basename "$TARBALL") for $PLATFORM_KEY..."

export TAURI_SIGNING_PRIVATE_KEY
[[ -n "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]] && export TAURI_SIGNING_PRIVATE_KEY_PASSWORD

pnpm tauri signer sign -k "$TAURI_SIGNING_PRIVATE_KEY" "$TARBALL"

SIG_FILE="${TARBALL}.sig"
if [[ ! -f "$SIG_FILE" ]]; then
  echo "Signature not produced: $SIG_FILE" >&2
  exit 1
fi
SIGNATURE=$(cat "$SIG_FILE" | tr -d '[:space:]')

mkdir -p "$OUT_DIR"
cp "$TARBALL" "$OUT_DIR/"
cp "$SIG_FILE" "$OUT_DIR/"

# Copy .dmg for direct download (optional but typical)
DMG_FILE=$(find "$DMG_DIR" -maxdepth 1 -name "*_${VERSION}_*.dmg" -print -quit 2>/dev/null || true)
[[ -n "$DMG_FILE" ]] && cp "$DMG_FILE" "$OUT_DIR/"

TARBALL_NAME=$(basename "$TARBALL")
URL="$BASE_URL/$TARBALL_NAME"

# Merge into existing latest.json (preserves windows-x86_64 entry from PS1 build)
MANIFEST="$OUT_DIR/latest.json"
PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

if [[ -f "$MANIFEST" ]] && command -v jq >/dev/null 2>&1; then
  jq --arg ver "$VERSION" \
     --arg notes "$NOTES" \
     --arg date "$PUB_DATE" \
     --arg plat "$PLATFORM_KEY" \
     --arg sig "$SIGNATURE" \
     --arg url "$URL" \
     '.version = $ver
      | .notes = $notes
      | .pub_date = $date
      | .platforms[$plat] = {signature: $sig, url: $url}' \
     "$MANIFEST" > "${MANIFEST}.tmp" && mv "${MANIFEST}.tmp" "$MANIFEST"
else
  cat > "$MANIFEST" <<EOF
{
  "version": "$VERSION",
  "notes": "$NOTES",
  "pub_date": "$PUB_DATE",
  "platforms": {
    "$PLATFORM_KEY": {
      "signature": "$SIGNATURE",
      "url": "$URL"
    }
  }
}
EOF
fi

echo ""
echo "Artifacts staged in: $OUT_DIR"
echo "Next step: create a GitHub release at https://github.com/LogixOrg/orcaa-desktop/releases tagged v$VERSION,"
echo "then upload all files from $OUT_DIR as release assets."
echo ""
ls -lh "$OUT_DIR"
