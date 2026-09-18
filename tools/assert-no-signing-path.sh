#!/bin/sh
# Asserts the device build of `paperctl` contains no code path that can sign a
# release (§12): the tablet installs and verifies, and must not hold a signing
# key.
#
# `ed25519_dalek::SigningKey` and `paper_packages::signing::SecretKey` (its
# only caller in this workspace) are both behind the `publishing` feature,
# which `--no-default-features` turns off — so this is normally a compile-time
# guarantee, not a runtime one. This script is the check that the guarantee
# actually held for the binary that was built: it greps the compiled artefact
# for the symbol names that signing would leave behind, so a future change
# that accidentally drops `#[cfg(feature = "publishing")]` somewhere fails
# here instead of shipping.
#
#   ./tools/assert-no-signing-path.sh <path-to-paperctl-binary>

set -eu

binary=${1:?"usage: assert-no-signing-path.sh <path-to-paperctl-binary>"}

[ -f "$binary" ] || {
    echo "assert-no-signing-path.sh: no binary at $binary" >&2
    exit 2
}

if nm -C "$binary" 2>/dev/null | grep -qi 'SigningKey\|SecretKey'; then
    echo "assert-no-signing-path.sh: found a signing symbol in $binary — §12 is violated" >&2
    nm -C "$binary" 2>/dev/null | grep -i 'SigningKey\|SecretKey' >&2
    exit 1
fi

echo "assert-no-signing-path.sh: $binary has no signing symbol"
