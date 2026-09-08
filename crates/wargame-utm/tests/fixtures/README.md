# `wargame-utm` fixtures

Raw, secret-free captures of the UTM automation surface, recorded on **2026-09-09** against
**UTM 4.7.5 (build 118)** on **macOS 26.6.2 (25G83), Apple Silicon**.

**Start with [`readback-transcript.txt`](readback-transcript.txt)** — one continuous
command/output/exit-code log covering create → attach a second ISO → replace an ISO → list-property
quirks → `qemu additional arguments` round-trip → network mode `host` with the host-side `ifconfig`
before and after → serial address → input automation → the full suspend/resume/stop status walk →
export/import (and the duplicate-name trap) → `hypervisor` true-vs-false on x86_64 with both the
configuration readback and the generated `-accel` → cleanup.

Two behaviours are captured elsewhere because they turn on *which stream* a line arrives on and on
the un-piped exit status, which that transcript's merged `2>&1` cannot show:
[`attach-and-default-drives.txt`](attach-and-default-drives.txt) (the `--attach` stub and the
drives UTM invents on its own) and [`orphan-qemu-process.txt`](orphan-qemu-process.txt) (trap T12,
a one-off). Every other file here is a single capture pulled out for direct matching.

**Matching rule:** UTM emits typographic apostrophes (U+2019) — `couldn’t`, `Can’t`. These files
are byte-exact. Match on the error **number** (`-1700`, `-2700`, `-1743`, …) or on an ASCII-only
substring such as `Error from event:` / `Error:` / `The virtual machine is not running.`
**Never match a sentence containing an apostrophe.**

There is no `wargame-utm` Cargo crate yet (Task 1 is verification only). These files exist so the
adapter written in a later task can be tested against real output instead of invented output.
Findings and the operation matrix live in [`docs/utm-compatibility.md`](../../../../docs/utm-compatibility.md).

Every file below was produced by the command listed next to it. Nothing was hand-edited except
where a "post-processing" note says so. All VMs used were app-owned temporaries named
`wargame-t1-*` and were deleted afterwards.

## Versions and app metadata

| File | Command | Exit |
|---|---|---|
| `utmctl-version.txt` | `utmctl version` | 0 |
| `utm-4.7.5-info-plist-excerpt.txt` | `plutil -convert xml1 -o - /Applications/UTM.app/Contents/Info.plist \| grep -A1 -E 'CFBundleShortVersionString\|CFBundleVersion\|LSMinimumSystemVersion\|CFBundleIdentifier'` | 0 |
| `utm-4.7.5-entitlements.plist` | `codesign -d --entitlements - --xml /Applications/UTM.app \| plutil -convert xml1 -o - -` | 0 |

## CLI surface

| File | Command | Exit |
|---|---|---|
| `utmctl-help.txt` | `utmctl --help` | 0 |
| `utmctl-subcommand-help.txt` | `for c in version list status start suspend stop attach file exec ip-address clone delete usb; do utmctl $c --help; done` | 0 each |
| `utmctl-list-multi.txt` | `utmctl list` (4 temp VMs registered) | 0 |
| `utmctl-list-empty.txt` | `utmctl list` after every temp VM was deleted — header row only | 0 |
| `utmctl-status-stopped.txt` | `utmctl status wargame-t1-kali-arm64` | 0 |
| `utmctl-status-started.txt` | `utmctl status wargame-t1-kali-arm64` while running | 0 |
| `utmctl-attach-not-implemented.txt` | `utmctl attach wargame-t1-kali-arm64` (running) | 0 — command is a stub; the stream-separated version is in `attach-and-default-drives.txt` §4 |

## Error shapes

`utmctl` writes all of these to **stderr**. Note the exit codes: only "Virtual machine not found"
is a non-zero exit. Apple-Event failures are reported on stderr with **exit 0**.

| File | Command | Exit |
|---|---|---|
| `utmctl-error-vm-not-found.stderr.txt` | `utmctl status wargame-t1-does-not-exist` | **1** |
| `utmctl-error-not-running.stderr.txt` | `utmctl stop wargame-t1-kali-arm64` while stopped | **0** |
| `utmctl-error-guest-agent-missing.stderr.txt` | `utmctl ip-address wargame-t1-kali-arm64` while running, no guest agent | **0** |
| `utmctl-error-orphan-qemu-lock.stderr.txt` | `utmctl start wargame-t1-attach` while an orphaned QEMU process from the previous `stop --kill` still held `efi_vars.fd` (trap T12; context in `orphan-qemu-process.txt`) | **0** |
| `applescript-error-vm-not-found.txt` | `osascript -e 'tell application "UTM" to return id of virtual machine named "wargame-t1-does-not-exist"'` | 1 |
| `applescript-error-appleevents-denied.txt` | `osascript -e 'tell application "System Events" to tell process "UTM" to return name of every window'` | 1 |
| `applescript-error-unknown-property-tpm.txt` | `update configuration … with (configuration & {tpm:true})` | 1 |
| `applescript-error-port-forward-protocol-tcp.txt` | port forward written as `{protocol:TCP, host port:22022, guest port:22}` | 1 |
| `applescript-error-vm-must-be-stopped.txt` | `set memory of c to 2048` + `update configuration` while the VM is running | 1 |

`applescript-error-appleevents-denied.txt` is the shape a **denied macOS Automation permission**
produces (`-1743`). It was captured against `System Events`, which this host has not granted, not
against UTM (UTM automation was already effective for the terminal in this session) — the error
number and wording are what UTM would produce if its own grant were missing.

## AppleScript dictionary and raw records

| File | Command |
|---|---|
| `utm-4.7.5.sdef` | `sdef /Applications/UTM.app` |
| `applescript-configuration-qemu-aarch64.txt` | `osascript applescript/dump-configuration.applescript wargame-t1-kali-arm64` |
| `applescript-configuration-qemu-x86_64.txt` | `osascript applescript/dump-configuration.applescript wargame-t1-kali-amd64` |

The two `applescript-configuration-*.txt` files are how `osascript` flattens the whole
`qemu configuration` record to text. They show which properties come back (and which do **not** —
`source` and `guest size` of a drive are never returned).

## Verified AppleScript snippets — `applescript/`

Each of these ran successfully on this host, and **each carries a header comment** giving its usage
and the traps it encodes (sandbox-relative `POSIX file`, the stopped-VM requirement, unreadable
drive fields, the `count of` quirk, the `TCP` terminology collision, the silently-ignored
`hypervisor`, and the fact that `duplicate` returns nothing).

| File | Purpose |
|---|---|
| `create-qemu-vm.applescript` | `make new virtual machine` with backend/arch/memory/cores/hypervisor/uefi + ISO + blank disk |
| `show-vm.applescript` | id, name and the scalar configuration fields |
| `dump-configuration.applescript` | returns the whole configuration record |
| `show-drives.applescript` | lists the drives with every field UTM actually returns |
| `set-network-mode.applescript` | switch between `shared` / `host` / `emulated` / `bridged` |
| `set-iso-source.applescript` | replace the ISO of an existing removable drive (and set `host` mode) |
| `add-removable-drive.applescript` | append a second CD drive (needed for the Windows guest-tools ISO) |
| `add-display.applescript` | add a display device (new VMs are created with **none**) |
| `clear-displays.applescript` | remove all displays so the console lands on the serial ptty |
| `duplicate-vm.applescript` | `duplicate … with properties`, then look the copy up by name |
| `serial-port-address.applescript` | `address of first serial port` → the host ptty path |
| `set-port-forward-raw-tcp-code.applescript` | TCP port forward using `«constant ****TcPp»` |

## Generated QEMU command lines

UTM 4.7.5 runs QEMU as a child of `QEMUHelper.xpc`, so the full argv is visible from the host.
Captured with the VM running:

```
ps -Aww -o command | grep 'qemu-aarch64-softmmu' | grep -v grep     # arm64 VMs
ps -Aww -o command | grep 'qemu-x86_64-softmmu'  | grep -v grep     # x86_64 VMs
```

| File | Configuration under test |
|---|---|
| `qemu-argv-aarch64-netmode-shared.txt` | `mode:shared` → `-netdev vmnet-shared,id=net0` |
| `qemu-argv-aarch64-netmode-host.txt` | `mode:host` → `-netdev vmnet-host,id=net0` |
| `qemu-argv-aarch64-netmode-emulated.txt` | `mode:emulated` + one forward → `-netdev user,id=net0,hostfwd=tcp::22022-:22` |
| `qemu-argv-aarch64-netmode-bridged.txt` | `mode:bridged, host interface:"en0"` → `-netdev vmnet-bridged,id=net0,ifname=en0` |
| `qemu-argv-x86_64-emulate-hypervisor-off.txt` | `architecture:"x86_64", hypervisor:false` → `-accel tcg,tb-size=1024` |
| `qemu-argv-x86_64-emulate-hypervisor-on-ignored.txt` | same VM with `hypervisor:true` → still `-accel tcg,tb-size=1024` |

## Consolidated logs and their drivers

| File | How |
|---|---|
| `readback-transcript.txt` | `./readback-transcript.sh > readback-transcript.txt 2>&1` — sections `## 0.` … `## 13.` plus `## 14.` (cleanup), cited by number from `docs/utm-compatibility.md`. Fully driver-captured; nothing hand-written. Because the driver merges stdout and stderr, anything that turns on *which* stream a line came from lives in its own fixture instead (next two rows). |
| `readback-transcript.sh` | the driver itself, with `AS` (defaults to `./applescript`) and `OUT` (defaults to `$TMPDIR`) parameterised. It creates two `wargame-t1-*` VMs; delete them by UUID afterwards. |
| `attach-and-default-drives.txt` | `./attach-probe.sh > attach-and-default-drives.txt 2>&1` — keeps stdout and stderr apart and records the un-piped exit status and wall-clock seconds. Covers (a) the two drives UTM invents when `make` omits the `drives` key, argv included, and (b) `utmctl attach` plus three consecutive `utmctl start --attach` runs. Ends with an ANOMALY note about two earlier `--attach` runs that blocked instead. |
| `attach-probe.sh` | that driver. Creates one VM, `wargame-t1-attach2`, and deletes it. |
| `orphan-qemu-process.txt` | trap T12. **One-off observation, transcribed command by command** — see its own PROVENANCE header. Every quoted line is byte-exact; the trigger is unknown and it has not been reproduced deliberately. |

## Boot evidence

| File | How |
|---|---|
| `boot-timing-kali-arm64-virtualize.txt` | `utmctl start` → poll `utmctl status` → read the serial ptty, timestamping the first firmware/GRUB match |
| `boot-timing-kali-amd64-emulate.txt` | same, x86_64 with `hypervisor:false` |
| `serial-boot-kali-arm64.txt` | serial capture, `tr -d '\000' \| cat -v` |
| `serial-boot-kali-amd64.txt` | serial capture, `tr -d '\000' \| cat -v` |
| `serial-grub-entry-kali-arm64.txt` | GRUB edit view showing the installer's kernel command line (`console=tty0`) |

The timing scripts are inline Python in `docs/utm-compatibility.md`'s reproduction section rather
than committed helpers, because they poke a live app.

## ISO records

| File | Command |
|---|---|
| `kali-2026.2-installer-sha256sums.txt` | `curl -sSL https://cdimage.kali.org/current/SHA256SUMS \| grep -E 'installer-(amd64\|arm64)\.iso$'` |
| `kali-2026.2-installer-sha256-local.txt` | `shasum -a 256 *.iso` in `~/Library/Application Support/wargame-vm/iso/` |

The ISOs themselves are **not** committed. They live under
`~/Library/Application Support/wargame-vm/iso/` on the host.
