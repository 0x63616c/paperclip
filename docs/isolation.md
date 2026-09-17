# What the isolation actually enforces

§11 chose crash recovery over hostile-app isolation. This file is where that
choice stops being a sentence and becomes something a reader can check, because
the failure mode it guards against is specific: a unit file full of impressive
directives, every one of them accepted by systemd, several of them enforcing
nothing at all.

Three verdicts, and the third is the one that matters:

| Verdict | Meaning |
|---|---|
| **enforced** | The kernel or systemd is holding it, and the VM harness demonstrated it. |
| **partial** | Something is in force, but it does not mean what its name suggests. |
| **ineffective** | The directive exists in systemd's vocabulary, is accepted without complaint, and does nothing here. **Paperclip does not write these.** |

A directive Paperclip will not enforce is not written into the unit, because a
unit file that lists it documents a protection that does not exist. Where a
mechanism is missing, the generator writes the closest thing that *is* enforced
and the omission appears here with its reason.

Generate the current table for any machine with:

```sh
paperctl isolation --target here        # probe this machine
paperctl isolation --target paper-pro   # the recorded device profile
```

## The reMarkable Paper Pro

Derived from the facilities WWW-1 and WWW-11 established on the device. This is
a transcription of a measurement, not a measurement — `paperctl isolation` says
so on its first line, and nothing in the code lets a recorded profile be
reported as a live probe.

| Protection | Verdict | In force | Do not assume |
|---|---|---|---|
| User separation | **enforced** | `User=` on the session unit; the vendor's own unprivileged uid | Same uid as stock Xochitl. A session can reach anything that uid can reach, including Xochitl's own files. This is separation from root, not from the user's data. |
| Writable storage | **partial** | `ProtectSystem=strict`, `ReadOnlyPaths=`, `ReadWritePaths=` from granted capabilities, `InaccessiblePaths=/data` | `/tmp` is writable, deliberately. Display ownership is registered in `/tmp/epframebuffer.lock` and `/tmp/epd.lock`, and a session with `PrivateTmp=` cannot join that registry. |
| Device access | **enforced** | `DevicePolicy=closed` with an explicit `DeviceAllow=` list | The list includes the DRM node and input devices, because a foreground session needs both. Any session that can draw can also read every pen and touch event. |
| Memory bound | **partial** | `LimitAS=` only — an rlimit, checked per process at allocation time | **`MemoryMax=` is not in force and is not written.** The memory controller is not delegated to our cgroup, so systemd accepts `MemoryAccounting=yes` and then reports `MemoryCurrent=[not set]`. Memory exhaustion is bounded per process by the rlimit and system-wide by the OOM killer. It is not bounded per session. |
| Task-count bound | **enforced** | `TasksMax=` on the session cgroup | Counts tasks, not memory or descriptors. |
| CPU bound | **partial** | `CPUQuota=`, when configured | Unset by default. A busy loop will not lock the device out of recovery — the supervisor's deadlines do not depend on the session yielding — but it will make the panel feel dead. |
| Log and output growth | **enforced** | `LogRateLimitIntervalSec=`, `LogRateLimitBurst=` | Bounds the journal. A session writing its own file inside its granted storage is bounded by the filesystem, not by this. |
| Syscall filtering | **ineffective** | nothing at the unit level | systemd on this image is built `-SECCOMP`, so `SystemCallFilter=` and every related directive are parsed and ignored. An app may install its own filter after exec, which protects against its own later bugs and not against the app itself. |
| Mandatory access control | **absent** | nothing | The only LSM is the `capability` stub. No SELinux, AppArmor, Smack or TOMOYO — so every restriction above is discretionary with no mandatory backstop. §11's refusal to claim hostile-code isolation rests on this row. |
| Process-tree containment | **enforced** | `KillMode=control-group`, plus a supervisor sweep of `cgroup.procs` repeated until empty | Not atomic. Neither `cgroup.freeze` nor `cgroup.kill` exists on this kernel, so a process that forks between two reads is caught on the next pass rather than the current one. The sweep repeats until the set stays empty, which terminates against anything short of a deliberate fork bomb. |
| Network reach | **ineffective** | nothing at the unit level | Both mechanisms are missing: `IPAddressDeny=` needs systemd's BPF framework (**unestablished on this device — one line of `systemctl --version` closes it**) and `RestrictAddressFamilies=` needs seccomp, which this systemd does not have. **An app that was not granted the network capability can still open a socket.** On this platform the capability is a record of a decision, not an enforced boundary. |
| Privilege escalation | **enforced** | `NoNewPrivileges=yes`, empty `CapabilityBoundingSet=`, `RestrictSUIDSGID=yes` | Kernel vulnerabilities are out of scope. |

### The one sentence to take away

On the Paper Pro, Paperclip contains **accidents**, not **adversaries**. A
buggy app cannot corrupt the root filesystem, cannot touch `/data`, cannot
outlive its own session, and cannot stop the display coming back. A *hostile*
app running as the session uid can read the user's notebooks, open a socket,
and exhaust memory. §11 says not to claim otherwise, and this table is what
that instruction looks like when it is carried out.

## What the harness proved, and where

`tests/failure-harness` runs in an aarch64 Debian VM under QEMU. Its
`directives-applied` case reads the values back out of systemd after a session
is running, so "written into the unit" and "in force" are separate claims and
the second one is checked.

Two things it deliberately does **not** prove:

- **Anything about the tablet.** The VM has a delegated memory controller,
  seccomp, and AppArmor; the Paper Pro has none of those. The harness's
  `facilities` case asserts the two profiles differ, precisely so a green VM
  run can never be quoted as a device result.
- **Isolation against hostile code**, anywhere. See the row above.

A container will not do. Both OrbStack and Docker run systemd inside LXC, and
LXC ships a generator that writes a global drop-in setting `ProtectSystem=no`,
`NoNewPrivileges=no` and empty `ReadWritePaths=` for *every* service on the
machine. A harness run there reports that nothing is enforced — and would have
reported exactly the same thing for a unit file that was genuinely wrong.

## Open questions for the device

| Question | Why it matters | How to close it |
|---|---|---|
| Is systemd on the Paper Pro built with the BPF framework? | It decides whether `IPAddressDeny=` is written, and therefore whether the network capability is a boundary or a note. | `systemctl --version` on the device; look for `+BPF_FRAMEWORK`. |
| What is systemd's version there? | Several directives changed behaviour across versions; the recorded profile currently says "unknown" rather than guessing. | The first line of the same command. |
| Does `LimitAS=` actually bound the vendor's Qt stack? | A Qt app maps a lot of address space it never touches. If the rlimit has to be set high enough for Qt, it bounds very little. | Run a session under it and read `VmPeak`. |
