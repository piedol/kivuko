#!/bin/zsh
# Build Kivuko.app (menu-bar app + VPN app extension) and sign it for this Mac.
#   scripts/bundle.sh             build + sign into dist/Kivuko.app
#   scripts/bundle.sh --install   also copy to /Applications and open it
#
# Kivuko is a personal build: it's signed with an Apple Development certificate and
# development provisioning profiles that cover only the Macs registered in the developer
# account. No notarization is needed. The profiles come from Xcode's automatic signing
# (signing/project.yml); this script refreshes them when they're missing or expire.
set -euo pipefail
cd "${0:A:h}/.."

# One place for the names (shared/src/lib.rs has the same values).
NAME=Kivuko
APP_ID=top.piedol.kivuko
EXT_ID=top.piedol.kivuko.tunnel
EXT_NAME=KivukoTunnel
TEAM=ABCDE12345
PROFILES=~/Library/Developer/Xcode/UserData/Provisioning\ Profiles

INSTALL=0
for a in "$@"; do
  case $a in
    --install) INSTALL=1 ;;
    *) echo "unknown option $a"; exit 2 ;;
  esac
done

# The newest unexpired development profile for a bundle ID, or nothing.
profile_for() {
  local want="$TEAM.$1" best="" best_exp="" p plist id exp
  for p in "$PROFILES"/*.provisionprofile(N); do
    plist=$(security cms -D -i "$p" 2>/dev/null) || continue
    id=$(plutil -extract Entitlements.com\\.apple\\.application-identifier raw - <<<"$plist" 2>/dev/null) || continue
    [[ $id == "$want" ]] || continue
    plutil -extract ProvisionedDevices raw - <<<"$plist" >/dev/null 2>&1 || continue   # development only
    exp=$(plutil -extract ExpirationDate raw - <<<"$plist")
    [[ $exp > $(date -u +%Y-%m-%dT%H:%M:%SZ) ]] || continue
    [[ -z $best || $exp > $best_exp ]] && { best=$p; best_exp=$exp; }
  done
  print -r -- "$best"
}

APP_PROFILE=$(profile_for $APP_ID)
EXT_PROFILE=$(profile_for $EXT_ID)
if [[ -z $APP_PROFILE || -z $EXT_PROFILE ]]; then
  echo "Getting development signing from Xcode…"
  (cd signing && xcodegen -q -s project.yml && xcodebuild -project KivukoSigning.xcodeproj -scheme Kivuko \
    -configuration Debug -derivedDataPath build -allowProvisioningUpdates -allowProvisioningDeviceRegistration \
    build -quiet) || { echo "Xcode couldn't create the signing. Is Xcode signed in (Settings › Accounts)?"; exit 1; }
  APP_PROFILE=$(profile_for $APP_ID)
  EXT_PROFILE=$(profile_for $EXT_ID)
fi
[[ -n $APP_PROFILE && -n $EXT_PROFILE ]] || { echo "no development profiles for $APP_ID / $EXT_ID"; exit 1; }

# The Apple Development certificate the profiles were made for.
IDENTITY=$(security cms -D -i "$APP_PROFILE" | plutil -extract DeveloperCertificates.0 raw - | base64 -d \
  | shasum -a 1 | cut -d' ' -f1 | tr a-f A-F)
security find-identity -v -p codesigning | grep -q "$IDENTITY" || { echo "the profile's certificate isn't in the keychain"; exit 1; }

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
fill() {
  sed -e "s/__NAME__/$NAME/g" -e "s/__APP_ID__/$APP_ID/g" -e "s/__EXT_ID__/$EXT_ID/g" -e "s/__EXT_NAME__/$EXT_NAME/g" \
      -e "s/__TEAM__/$TEAM/g" -e "s/__VERSION__/$VERSION/g" -e "s/__BUILD__/$BUILD/g" "$1" > "$2"
}

cargo build --release --target aarch64-apple-darwin -p app -p provider
OUT=target/aarch64-apple-darwin/release

APP=dist/$NAME.app
EXT=$APP/Contents/PlugIns/$EXT_NAME.appex
rm -rf "$APP" dist/tmp
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$EXT/Contents/MacOS" dist/tmp
cp "$OUT/$NAME" "$APP/Contents/MacOS/$NAME"
cp "$OUT/provider" "$EXT/Contents/MacOS/$EXT_NAME"
fill resources/Info.plist "$APP/Contents/Info.plist"
fill resources/Provider-Info.plist "$EXT/Contents/Info.plist"
fill resources/App.entitlements dist/tmp/app.entitlements
fill resources/Provider.entitlements dist/tmp/provider.entitlements
cp "$APP_PROFILE" "$APP/Contents/embedded.provisionprofile"
cp "$EXT_PROFILE" "$EXT/Contents/embedded.provisionprofile"
[[ -f assets/AppIcon.icns ]] && cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

# Inside out: the extension, then the app.
sign() { codesign --force --options runtime --timestamp=none --sign "$IDENTITY" "$@"; }
sign --entitlements dist/tmp/provider.entitlements "$EXT"
sign --entitlements dist/tmp/app.entitlements "$APP"
codesign --verify --strict --deep "$APP"
rm -rf dist/tmp
echo "Built $APP ($(du -sh "$APP" | cut -f1); app $(du -h "$APP/Contents/MacOS/$NAME" | cut -f1), tunnel $(du -h "$EXT/Contents/MacOS/$EXT_NAME" | cut -f1))"

if (( INSTALL )); then
  pkill -x "$NAME" 2>/dev/null || true
  rm -rf "/Applications/$NAME.app"
  ditto "$APP" "/Applications/$NAME.app"
  open "/Applications/$NAME.app"
  echo "Installed to /Applications/$NAME.app"
fi
