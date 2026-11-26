#!/usr/bin/env bash
#
# Sign a standalone macOS executable (cwbcode, cwbcodeclient), and notarize it when there is a
# real identity to notarize with. The environment is the one FruitNationMessenger's scripts and
# Tauri read, so one set of credentials covers the app and the binaries:
#
#   APPLE_SIGNING_IDENTITY  Developer ID Application: …   (default: the keychain's, else `-`)
#   APPLE_ID                the Apple ID for notarytool
#   APPLE_PASSWORD          its app-specific password
#   APPLE_TEAM_ID           the team the certificate belongs to
#   APPLE_NOTARY_PROFILE    instead of the three above: a notarytool keychain profile
#
# A ticket cannot be stapled to a bare executable, so a notarized binary costs the first Mac that
# runs it one online check with Apple.
set -euo pipefail
BIN="${1:?usage: codesign-binary.sh <path-to-executable>}"
[[ -f "$BIN" ]] || { echo "error: no such file: $BIN" >&2; exit 1; }
IDENTITY="${APPLE_SIGNING_IDENTITY:-$("$(dirname "$0")/apple-identity.sh")}"
if [[ "$IDENTITY" == "-" ]]; then
    # Apple Silicon needs some signature to run at all.
    echo "  signing $(basename "$BIN") ad hoc"
    codesign --force --sign - "$BIN"
    exit 0
fi
# The hardened runtime (which notarization requires) and a secure timestamp (without it the
# signature ends with the certificate).
echo "  signing $(basename "$BIN") as $IDENTITY"
codesign --force --options runtime --timestamp --sign "$IDENTITY" "$BIN"
codesign --verify --strict "$BIN"
if [[ -n "${APPLE_NOTARY_PROFILE:-}" ]]; then
    NOTARY=(--keychain-profile "$APPLE_NOTARY_PROFILE")
elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
    NOTARY=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
else
    echo "  note: no notarization credentials, $(basename "$BIN") is signed only"
    exit 0
fi
ZIP="$(mktemp -d)/$(basename "$BIN").zip"
trap 'rm -rf "$(dirname "$ZIP")"' EXIT
ditto -c -k "$BIN" "$ZIP"   # notarytool takes an archive, never a loose executable
echo "  notarizing $(basename "$BIN")"
out=$(xcrun notarytool submit "$ZIP" "${NOTARY[@]}" --wait --timeout 30m 2>&1) || true
echo "$out" | sed 's/^/    /' | tail -6
grep -q "status: Accepted" <<<"$out" || { echo "error: notarization of $(basename "$BIN") was not accepted (xcrun notarytool log <id> …)" >&2; exit 1; }
echo "  notarized $(basename "$BIN")"
