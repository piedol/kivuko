#!/bin/zsh
# Build Kivuko.app (menu-bar app + VPN app extension) and sign it for this Mac.
#   scripts/bundle.sh             build + sign into dist/Kivuko.app
#   scripts/bundle.sh --install   also copy to /Applications and open it
#   scripts/bundle.sh --check     print the team and bundle IDs, write the Info.plists and
#                                 entitlements to dist/check/ and stop: nothing is built,
#                                 signed or registered, and Xcode isn't run
#
# Signing needs your Apple team and a bundle-ID prefix you own (bundle IDs are unique
# across all developer accounts):
#   KIVUKO_TEAM            your team ID. Default: the team of the one Apple Development
#                          certificate in your login keychain.
#   KIVUKO_BUNDLE_PREFIX   e.g. com.yourname, giving com.yourname.kivuko and
#                          com.yourname.kivuko.tunnel. Default: top.piedol, the project's
#                          own, which only its owner can sign.
# Set them in the environment, or once in signing/local.env (git-ignored; copy
# signing/local.env.example). The environment wins over the file.
#
# Kivuko is signed with an Apple Development certificate and development provisioning
# profiles that cover only the Macs registered in the developer account. No notarization is
# needed. The profiles come from Xcode's automatic signing (signing/project.yml); this script
# refreshes them when they're missing or expire.
set -euo pipefail
cd "${0:A:h}/.."

INSTALL=0 CHECK=0
for a in "$@"; do
  case $a in
    --install) INSTALL=1 ;;
    --check) CHECK=1 ;;
    *) echo "unknown option $a"; exit 2 ;;
  esac
done

# The team of the one Apple Development signing identity in the login keychain. The team ID
# is the certificate subject's OU; the ID in parentheses after your name is not the team.
detect_team() {
  local keychain=~/Library/Keychains/login.keychain-db certs line sha="" pem="" subject ou cn
  local -a valid teams names
  # SHA-1s of the valid Apple Development identities (certificate plus private key).
  valid=(${(f)"$(security find-identity -v -p codesigning "$keychain" 2>/dev/null \
    | awk '/"Apple Development: / { print $2 }' || true)"})
  certs=$(security find-certificate -a -Z -p -c "Apple Development: " "$keychain" 2>/dev/null) || true
  while IFS= read -r line; do
    case $line in
      "SHA-1 hash: "*) sha=${line#SHA-1 hash: } ;;
      -----BEGIN*) pem=$line$'\n' ;;
      -----END*)
        pem+=$line$'\n'
        if (( ${valid[(Ie)$sha]} )); then
          subject=$(openssl x509 -noout -subject -nameopt sep_multiline,sname,utf8 <<<"$pem") || continue
          ou=$(sed -n 's/^ *OU=//p' <<<"$subject" | head -1)
          cn=$(sed -n 's/^ *CN=//p' <<<"$subject" | head -1)
          if [[ -n $ou ]] && (( ! ${teams[(Ie)$ou]} )); then teams+=$ou; names+=$cn; fi
        fi ;;
      *) pem+=$line$'\n' ;;
    esac
  done <<<"$certs"
  case ${#teams} in
    1) TEAM=$teams[1]; echo "Team $TEAM, from the certificate \"$names[1]\"" ;;
    0) echo "No Apple Development certificate in your login keychain, so the team is unknown."
       echo "Set KIVUKO_TEAM to your team ID (developer.apple.com/account › Membership details), or"
       echo "create the certificate in Xcode › Settings › Accounts › Manage Certificates."
       exit 1 ;;
    *) echo "Apple Development certificates for several teams are in your login keychain:"
       for i in {1..${#teams}}; do echo "  $teams[i]  ($names[i])"; done
       echo "Set KIVUKO_TEAM to the one to sign with."
       exit 1 ;;
  esac
}

team=${KIVUKO_TEAM-} prefix=${KIVUKO_BUNDLE_PREFIX-}
[[ -f signing/local.env ]] && source signing/local.env
TEAM=${team:-${KIVUKO_TEAM-}}
PREFIX=${prefix:-${KIVUKO_BUNDLE_PREFIX:-top.piedol}}
[[ -n $TEAM ]] || detect_team
[[ $TEAM =~ '^[A-Z0-9]{10}$' ]] || { echo "KIVUKO_TEAM must be a 10-character team ID like ABCDE12345, not \"$TEAM\""; exit 1; }
[[ $PREFIX =~ '^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*$' ]] \
  || { echo "KIVUKO_BUNDLE_PREFIX must be reverse-DNS letters, digits and hyphens, like com.yourname"; exit 1; }

# The names. shared/src/lib.rs reads the bundle IDs from KIVUKO_APP_ID and KIVUKO_EXT_ID at
# compile time, and signing/project.yml takes them and the team from the environment.
NAME=Kivuko
APP_ID=$PREFIX.kivuko
EXT_ID=$APP_ID.tunnel
EXT_NAME=KivukoTunnel
export KIVUKO_TEAM=$TEAM KIVUKO_APP_ID=$APP_ID KIVUKO_EXT_ID=$EXT_ID
echo "Signing for team $TEAM as $APP_ID (tunnel $EXT_ID)"
PROFILES=~/Library/Developer/Xcode/UserData/Provisioning\ Profiles
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=$(git rev-list --count HEAD 2>/dev/null || echo 1)
fill() {
  sed -e "s/__NAME__/$NAME/g" -e "s/__APP_ID__/$APP_ID/g" -e "s/__EXT_ID__/$EXT_ID/g" -e "s/__EXT_NAME__/$EXT_NAME/g" \
      -e "s/__TEAM__/$TEAM/g" -e "s/__VERSION__/$VERSION/g" -e "s/__BUILD__/$BUILD/g" "$1" > "$2"
}

if (( CHECK )); then
  rm -rf dist/check
  mkdir -p dist/check
  fill resources/Info.plist dist/check/app-Info.plist
  fill resources/Provider-Info.plist dist/check/tunnel-Info.plist
  fill resources/App.entitlements dist/check/app.entitlements
  fill resources/Provider.entitlements dist/check/tunnel.entitlements
  echo "Wrote the Info.plists and entitlements to dist/check/. Nothing was built or signed."
  exit 0
fi

# Only one copy of the app may stay registered with macOS. With two copies of the tunnel
# extension (same bundle ID), launching the app makes macOS "uninstall" one of them and
# stop every running instance, which drops the VPN.
forget() { [[ -e $1 ]] || return 0; "$LSREGISTER" -u "$1" 2>/dev/null; rm -rf "$1"; }

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
  # Only the profiles were wanted; the stub app must not stay registered.
  forget signing/build/Build/Products/Debug/Kivuko.app
  rm -rf signing/build
  APP_PROFILE=$(profile_for $APP_ID)
  EXT_PROFILE=$(profile_for $EXT_ID)
fi
[[ -n $APP_PROFILE && -n $EXT_PROFILE ]] || { echo "no development profiles for $APP_ID / $EXT_ID"; exit 1; }

# The Apple Development certificate the profiles were made for.
IDENTITY=$(security cms -D -i "$APP_PROFILE" | plutil -extract DeveloperCertificates.0 raw - | base64 -d \
  | shasum -a 1 | cut -d' ' -f1 | tr a-f A-F)
security find-identity -v -p codesigning | grep -q "$IDENTITY" || { echo "the profile's certificate isn't in the keychain"; exit 1; }

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
  # A copied bundle can carry an access-control tag (com.apple.macl) that makes macOS
  # re-check and re-register the app on every launch, which restarts the tunnel.
  xattr -dr com.apple.macl "/Applications/$NAME.app" 2>/dev/null || true
  forget "$APP"
  open "/Applications/$NAME.app"
  # The first launch can add the tag again; clear it once the app is running.
  sleep 3
  xattr -dr com.apple.macl "/Applications/$NAME.app" 2>/dev/null || true
  echo "Installed to /Applications/$NAME.app"
fi
echo "Logs: log stream --predicate 'subsystem == \"$APP_ID\"' --level info"
