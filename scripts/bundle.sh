#!/bin/zsh
# Build Tamka.app (Rust menu-bar app + Swift speech engine helper) and sign it.
#   scripts/bundle.sh             build + sign into dist/Tamka.app
#   scripts/bundle.sh --install   also copy to /Applications and relaunch
#   scripts/bundle.sh --notarize  also notarize and zip for release (keychain profile $NOTARY_PROFILE, default piedol-notary)
set -euo pipefail
cd "${0:A:h}/.."

INSTALL=0; NOTARIZE=0
for a in "$@"; do
  case $a in
    --install) INSTALL=1 ;;
    --notarize) NOTARIZE=1 ;;
    *) echo "unknown option $a"; exit 2 ;;
  esac
done

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
# Sign with $TAMKA_SIGN_IDENTITY, or else the first Developer ID Application certificate
# in the keychain. Without either, the app gets an ad-hoc signature.
# Notarization needs a secure timestamp; everyday builds skip it to stay fast and work offline.
TS=--timestamp=none; (( NOTARIZE )) && TS=--timestamp
IDENTITY=${TAMKA_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p' | head -1)}

# Keep build-machine paths (home folder, checkout) out of the binary's panic messages.
# The encoded form separates flags with 0x1f, so paths with spaces work.
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$HOME/.cargo=/cargo"$'\x1f'"--remap-path-prefix=$PWD=/tamka"
cargo build --release --target aarch64-apple-darwin
# The same for Swift's #file paths in the engine.
(cd engine && swift build -c release -Xswiftc -file-prefix-map -Xswiftc "$PWD=/tamka-engine" -Xswiftc -file-prefix-map -Xswiftc "$HOME=/home")

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
  if [[ -n $IDENTITY ]] && security find-identity -v -p codesigning | grep -qF -- "\"$IDENTITY\""; then
    codesign --force --options runtime "$TS" --sign "$IDENTITY" "$@"
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

if (( NOTARIZE )); then
  # Notarization needs a keychain profile made once with:
  #   xcrun notarytool store-credentials <profile> --apple-id <Apple ID> --team-id <team ID>
  PROFILE=${NOTARY_PROFILE:-piedol-notary}
  ditto -c -k --keepParent "$APP" dist/Tamka-notarize.zip
  xcrun notarytool submit dist/Tamka-notarize.zip --keychain-profile "$PROFILE" --wait
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  rm -f dist/Tamka-notarize.zip
  ditto -c -k --keepParent "$APP" dist/Tamka-macOS-arm64.zip
  echo "Notarized: dist/Tamka-macOS-arm64.zip"
fi

if (( INSTALL )); then
  pkill -x Tamka 2>/dev/null || true
  rm -rf /Applications/Tamka.app
  ditto "$APP" /Applications/Tamka.app
  open /Applications/Tamka.app
  echo "Installed to /Applications/Tamka.app"
fi
