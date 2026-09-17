#!/bin/sh
# Copy libqsgepaper.so off the tablet, read-only, and record its provenance.
#
#   ./pull-vendor-lib.sh ~/paperclip-vendor [ssh-host]
#
# The library is proprietary (LICENSE: CLOSED). It is linkable on the device and
# must never be committed to this repository, so this copies it to a directory
# you name, outside the tree, and writes a provenance file next to it.
#
# Nothing on the tablet is modified: three reads and a copy. xochitl is not
# stopped, no service is touched, nothing is written to the device.
#
# No `set -e`: every step should report rather than abort half way.

dest=$1
host=${2:-remarkable-wifi}
lib=/usr/lib/plugins/scenegraph/libqsgepaper.so

if [ -z "$dest" ]; then
    echo "usage: $0 <destination-directory> [ssh-host]" >&2
    echo "  the destination must be OUTSIDE this repository" >&2
    exit 2
fi

mkdir -p "$dest" || exit 1

# Refuse to land inside a git work tree: the whole point is that this file
# never becomes a commit.
if (cd "$dest" && git rev-parse --is-inside-work-tree >/dev/null 2>&1); then
    echo "refusing: $dest is inside a git work tree, and this library must not be committed" >&2
    exit 1
fi

echo "reading device metadata from $host"
version=$(ssh "$host" 'cat /etc/version' 2>/dev/null)
image=$(ssh "$host" 'grep -h IMG_VERSION /etc/os-release 2>/dev/null' 2>/dev/null)
remote_sum=$(ssh "$host" "sha256sum $lib" 2>/dev/null)
licence=$(ssh "$host" 'cat /usr/share/common-licenses/libqsgepaper/recipeinfo 2>/dev/null' 2>/dev/null)

if [ -z "$remote_sum" ]; then
    echo "could not read $lib on $host — is the tablet awake and on Wi-Fi?" >&2
    echo "USB CDC does not survive autosleep; Wi-Fi does (WWW-20)." >&2
    exit 1
fi

echo "copying $lib"
scp "$host:$lib" "$dest/libqsgepaper.so" || exit 1

local_sum=$(shasum -a 256 "$dest/libqsgepaper.so" 2>/dev/null | awk '{print $1}')
[ -n "$local_sum" ] || local_sum=$(sha256sum "$dest/libqsgepaper.so" 2>/dev/null | awk '{print $1}')
remote_only=$(echo "$remote_sum" | awk '{print $1}')

{
    echo "# libqsgepaper.so provenance"
    echo "pulled-at:  $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
    echo "host:       $host"
    echo "path:       $lib"
    echo "version:    $version"
    echo "image:      $image"
    echo "sha256:     $local_sum"
    echo "licence:    $licence"
    echo
    echo "Proprietary. Do not commit, do not redistribute."
    echo "Re-run platform/device/native/check-abi.sh against this file after"
    echo "every OS update: SWUpdate replaces the whole rootfs slot."
} > "$dest/libqsgepaper.provenance.txt"

if [ "$local_sum" = "$remote_only" ]; then
    echo "OK: sha256 $local_sum matches the device"
else
    echo "MISMATCH: device $remote_only, local $local_sum" >&2
    exit 1
fi

echo "wrote $dest/libqsgepaper.so and its provenance"
echo "next: platform/device/native/check-abi.sh $dest/libqsgepaper.so"
