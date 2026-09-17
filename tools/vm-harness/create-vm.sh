#!/bin/sh
# Builds the aarch64 Debian VM the WWW-4 failure harness runs in.
#
# Usage: tools/vm-harness/create-vm.sh <directory>
#
# Everything lands in <directory>, including a private SSH key. Keep it out of
# the repository.
set -e

home=${1:?usage: create-vm.sh <directory>}
image_url=https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-genericcloud-arm64.qcow2
firmware=/opt/homebrew/share/qemu/edk2-aarch64-code.fd

command -v qemu-system-aarch64 >/dev/null || {
  echo "create-vm: qemu-system-aarch64 is not on PATH (brew install qemu)" >&2
  exit 1
}
[ -f "$firmware" ] || {
  echo "create-vm: no UEFI firmware at $firmware" >&2
  exit 1
}

mkdir -p "$home/seed"
cd "$home"

[ -f debian-base.qcow2 ] || curl -fsSL -o debian-base.qcow2 "$image_url"
[ -f harness_key ] || ssh-keygen -t ed25519 -N "" -f harness_key -C paperclip-harness >/dev/null

cat > seed/meta-data <<META
instance-id: paperclip-harness-001
local-hostname: paperclip-harness
META
cat > seed/user-data <<USER
#cloud-config
users:
  - name: harness
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    ssh_authorized_keys:
      - $(cat harness_key.pub)
ssh_pwauth: false
USER

# cloud-init finds its seed by the `cidata` volume label.
rm -f seed.iso
hdiutil makehybrid -o seed.iso -iso -joliet -default-volume-name cidata seed >/dev/null

[ -f vars.fd ] || dd if=/dev/zero of=vars.fd bs=1m count=64 2>/dev/null
[ -f debian.qcow2 ] || {
  qemu-img create -f qcow2 -F qcow2 -b "$home/debian-base.qcow2" debian.qcow2 20G >/dev/null
}

cat > run-vm.sh <<'RUN'
#!/bin/sh
set -e
here=$(cd "$(dirname "$0")" && pwd)
exec qemu-system-aarch64 \
  -M virt -cpu host -accel hvf -smp 4 -m 4096 \
  -drive if=pflash,format=raw,readonly=on,file=/opt/homebrew/share/qemu/edk2-aarch64-code.fd \
  -drive if=pflash,format=raw,file="$here/vars.fd" \
  -drive if=virtio,format=qcow2,file="$here/debian.qcow2" \
  -drive if=virtio,format=raw,media=cdrom,file="$here/seed.iso" \
  -netdev user,id=n0,hostfwd=tcp:127.0.0.1:2222-:22 \
  -device virtio-net-pci,netdev=n0 \
  -device virtio-rng-pci \
  -nographic -serial file:"$here/console.log" -monitor none
RUN
cat > vmsh <<'SH'
#!/bin/sh
exec ssh -i "$(dirname "$0")/harness_key" -p 2222 -o StrictHostKeyChecking=no \
  -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR harness@127.0.0.1 "$@"
SH
cat > vmcp <<'CP'
#!/bin/sh
exec scp -i "$(dirname "$0")/harness_key" -P 2222 -o StrictHostKeyChecking=no \
  -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR "$@"
CP
chmod +x run-vm.sh vmsh vmcp

if ./vmsh 'true' 2>/dev/null; then
  echo "create-vm: already running"
  exit 0
fi

nohup ./run-vm.sh > qemu.out 2>&1 &
echo "create-vm: booting, waiting for ssh on 127.0.0.1:2222"
attempt=0
while [ "$attempt" -lt 60 ]; do
  if ./vmsh 'true' 2>/dev/null; then
    ./vmsh 'echo "create-vm: up -- $(uname -srm), $(systemd-detect-virt)"'
    exit 0
  fi
  attempt=$((attempt + 1))
  sleep 5
done
echo "create-vm: the VM did not come up; see $home/console.log" >&2
exit 1
