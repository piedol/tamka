#!/bin/zsh
# Build Tamka.app (Rust menu-bar app + Swift speech engine helper) and sign it.
#   scripts/bundle.sh             build + sign into dist/Tamka.app
#   scripts/bundle.sh --install   also copy to /Applications and relaunch
set -euo pipefail
cd "${0:A:h}/.."

INSTALL=0
for a in "$@"; do
  case $a in
    --install) INSTALL=1 ;;
    *) echo "unknown option $a"; exit 2 ;;
  esac
done

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
IDENTITY=${TAMKA_SIGN_IDENTITY:-"Developer ID Application: Your Name (TEAMID)"}

cargo build --release --target aarch64-apple-darwin
(cd engine && swift build -c release)

APP=dist/Tamka.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Resources"
cp target/aarch64-apple-darwin/release/Tamka "$APP/Contents/MacOS/Tamka"
cp engine/.build/release/tamka-engine "$APP/Contents/Helpers/tamka-engine"
# FluidAudio_FluidAudio.bundle is left out on purpose: only FluidAudio's
# text-to-speech code reads it (Bundle.module in LuxTtsG2p), never speech-to-text.
sed -e "s/__VERSION__/$VERSION/" -e "s/__BUILD__/$BUILD/" resources/Info.plist > "$APP/Contents/Info.plist"
[[ -f assets/AppIcon.icns ]] || swift scripts/make-icon.swift
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cp assets/MenuIcon.png assets/MenuIcon@2x.png "$APP/Contents/Resources/"

sign() {
  if security find-identity -v -p codesigning | grep -q "$IDENTITY"; then
    codesign --force --options runtime --timestamp=none --sign "$IDENTITY" "$@"
  else
    echo "warning: signing identity not found, using ad-hoc signature (permissions reset on every build)"
    codesign --force --options runtime --sign - "$@"
  fi
}
# Inside out: helper first, then the app with its entitlements.
sign --identifier top.piedol.tamka.engine "$APP/Contents/Helpers/tamka-engine"
sign --entitlements resources/Tamka.entitlements "$APP"
codesign --verify --strict --deep "$APP"

echo "Built $APP ($(du -sh "$APP" | cut -f1); app $(du -h "$APP/Contents/MacOS/Tamka" | cut -f1), engine $(du -h "$APP/Contents/Helpers/tamka-engine" | cut -f1))"

if (( INSTALL )); then
  pkill -x Tamka 2>/dev/null || true
  rm -rf /Applications/Tamka.app
  ditto "$APP" /Applications/Tamka.app
  open /Applications/Tamka.app
  echo "Installed to /Applications/Tamka.app"
fi
