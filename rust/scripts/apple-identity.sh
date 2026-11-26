#!/usr/bin/env bash
# The "Developer ID Application: …" identity in the keychain to sign with, or `-` (ad hoc) when
# there is none. (As FruitNationMessenger's scripts/apple-identity.sh.)
security find-identity -v -p codesigning 2>/dev/null \
  | sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | head -1 | grep . || echo -
