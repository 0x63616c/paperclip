#!/bin/sh
# Builds the Linux binaries and runs the failure harness inside the VM.
#
# Usage: tools/vm-harness/run-harness.sh <vm-directory> [report-path]
set -e

home=${1:?usage: run-harness.sh <vm-directory> [report-path]}
report=${2:-docs/device/www-4-failure-harness.md}
repo=$(cd "$(dirname "$0")/../.." && pwd)

"$home/vmsh" 'command -v cargo >/dev/null' || {
  echo "run-harness: installing rust in the VM (once)"
  "$home/vmsh" 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null 2>&1'
  "$home/vmsh" 'sudo apt-get update -qq && sudo apt-get install -y -qq build-essential' >/dev/null 2>&1
}

# `--no-default-features` for paperctl: the device has no windowing stack, and
# the recovery entry point has to build without one.
"$home/vmsh" 'rm -rf ~/paperclip-src && mkdir -p ~/paperclip-src'
tar --exclude target --exclude .git -cf - -C "$repo" . | "$home/vmsh" 'tar -xf - -C ~/paperclip-src'
"$home/vmsh" 'set -e
  export PATH="$HOME/.cargo/bin:$PATH"
  # The repo pins a zig cross-linker for aarch64-unknown-linux-gnu, which is
  # the *host* triple in here. Use the native linker instead.
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=cc
  cd ~/paperclip-src
  cargo build --release -p paper-host -p paper-fault-app -p paper-failure-harness
  cargo build --release -p paperctl --no-default-features
  mkdir -p ~/bin
  for b in paperclip-host paperctl paper-fault-app paperclip-failure-harness; do
    rm -f ~/bin/$b && cp target/release/$b ~/bin/$b
  done'
"$home/vmsh" 'sudo ~/bin/paperclip-failure-harness --bin-dir ~/bin --report ~/harness-report.md'
"$home/vmcp" harness@127.0.0.1:harness-report.md "$repo/$report"
echo "run-harness: report written to $report"
