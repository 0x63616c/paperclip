# The VM the failure harness runs in

`tests/failure-harness` needs an aarch64 Linux machine with real systemd. These
scripts build one on a Mac with QEMU, from a stock Debian cloud image.

## Why not a container

Because a container cannot answer the question. OrbStack and Docker both run
systemd inside LXC, and LXC installs a systemd *generator* that writes
`/run/systemd/system/service.d/zzz-lxc-service.conf` for every service on the
machine:

```
[Service]
ProtectSystem=no
NoNewPrivileges=no
ProtectHome=no
PrivateTmp=no
ReadWritePaths=
ReadOnlyPaths=
```

A harness run there reports that none of §11's filesystem boundaries hold — and
would report exactly the same thing for a unit file that was genuinely wrong.
Worse, it re-appears on every `daemon-reload`, so it cannot be removed. This was
found the expensive way, by running the harness in a container first.

The VM here reports `systemd-detect-virt: qemu`, has no such drop-in, and
enforces the directives. The harness's `write-outside-grants` case is the one
that tells the two apart.

## Setting it up

Needs `qemu-system-aarch64` (`brew install qemu`) and about 2 GB of disk.

```sh
tools/vm-harness/create-vm.sh ~/paperclip-vm    # downloads, seeds, boots, waits for SSH
```

It writes everything into the directory you name — image, firmware variables,
an SSH key, and the wrapper scripts below. **Keep that directory out of the
repository**: it contains a private key.

```sh
~/paperclip-vm/vmsh 'uname -a'        # a shell in the VM
~/paperclip-vm/vmcp file harness@127.0.0.1:                 # copy into it
```

## Running the harness

```sh
tools/vm-harness/run-harness.sh ~/paperclip-vm
```

That cross-builds nothing: it builds the four Linux binaries on any Linux
machine of the same architecture you have to hand (the script uses the VM
itself if Rust is installed there, and otherwise tells you what to do), copies
them in, and runs:

```sh
sudo paperclip-failure-harness --bin-dir ~/bin --report www-4-harness.md
```

The report it writes is the evidence. It names the kernel, the systemd build
and the architecture, and it states on its own first page that it is not the
tablet.

## Shutting it down

```sh
~/paperclip-vm/vmsh 'sudo poweroff'
```

The VM is disposable. Delete the directory and re-run `create-vm.sh` to get a
clean one; nothing in the repository depends on its state.
