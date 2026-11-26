#!/usr/bin/env bash
#
# Sign a disk image, and notarize and staple it when there are credentials (the app inside is
# already signed, notarized and stapled by Tauri), so that the image and the app both open
# without a warning once downloaded. The environment is codesign-binary.sh's.
set -euo pipefail
DMG="${1:?usage: sign-dmg.sh <path-to.dmg>}"
IDENTITY="${APPLE_SIGNING_IDENTITY:-$("$(dirname "$0")/apple-identity.sh")}"
if [[ "$IDENTITY" == "-" ]]; then
    echo "  $(basename "$DMG"): no Developer ID, left unsigned (the app inside is signed ad hoc)"
    exit 0
fi
echo "  signing $(basename "$DMG") as $IDENTITY"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"
if [[ -n "${APPLE_NOTARY_PROFILE:-}" ]]; then
    NOTARY=(--keychain-profile "$APPLE_NOTARY_PROFILE")
elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
    NOTARY=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
else
    echo "  note: no notarization credentials, $(basename "$DMG") is signed but NOT notarized"
    exit 0
fi
echo "  notarizing $(basename "$DMG") (usually a few minutes)"
out=$(xcrun notarytool submit "$DMG" "${NOTARY[@]}" --wait --timeout 30m 2>&1) || true
echo "$out" | sed 's/^/    /' | tail -6
grep -q "status: Accepted" <<<"$out" || { echo "error: notarization of $(basename "$DMG") was not accepted (xcrun notarytool log <id> …)" >&2; exit 1; }
xcrun stapler staple "$DMG"
echo "  notarized and stapled $(basename "$DMG")"
