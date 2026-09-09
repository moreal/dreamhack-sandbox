# UTM compatibility record — 2026-09-09 (Task 1)

Verification-only pass for the multi-OS feature (`.superpowers/sdd/PLAN/spec.md` §3 FR-1, FR-2,
FR-5, FR-6). **No product code was written for this task.** Everything below was executed on this
host; raw output is committed under
[`crates/wargame-utm/tests/fixtures/`](../crates/wargame-utm/tests/fixtures/README.md), which also
lists the command that produced each file.

Claims are marked **verified** (a command in this document produced the quoted output),
**unverified** (documented by UTM/Kali/Microsoft but not reproduced here) or **blocked**
(attempted and prevented). Nothing is asserted without one of those labels.

**Amended 2026-09-09 (Task 7 phase B2a)** with what the first real Kali installations proved and
disproved: a drive-detach row and an input-automation *downgrade* in §2, and three console findings
in §3. Every amendment says so where it stands and cites
`.superpowers/sdd/PLAN/task-7-phase-a-report.md`, so the task-1 record and what came later stay
tellable apart.

## 1. Host and versions

| Item | Value | How |
|---|---|---|
| Host OS | macOS 26.6.2, build 25G83 | `sw_vers` |
| Host arch | `arm64` (Apple Silicon), 10 CPUs, 64 GiB RAM | `uname -m`, `sysctl hw.ncpu hw.memsize` |
| Free disk before | ~85 GiB (~68 GiB after the two ISOs) | `df -h /` |
| UTM | **4.7.5**, `CFBundleVersion` 118, bundle id `com.utmapp.UTM` | `utmctl version`, `Info.plist` |
| UTM install | `brew install --cask utm` → `Cask utm (4.7.5)`, `Moving App 'UTM.app' to '/Applications/UTM.app'`, `Linking Binary 'utmctl' to '/opt/homebrew/bin/utmctl'` | Homebrew cask, 1.2 GB in `/opt/homebrew/Caskroom/utm/4.7.5` |
| `utmctl` | `/Applications/UTM.app/Contents/MacOS/utmctl`, symlinked by the cask to `/opt/homebrew/bin/utmctl` | `ls -la`, `type -a utmctl` |
| Tart (unchanged) | `/opt/homebrew/bin/tart` | pre-existing |
| `qemu-img` | mise shim at `~/.local/share/mise/shims/qemu-img` — **not needed** for anything verified here | `which qemu-img` |

`utmctl` is a thin Apple-Events client for the app, so it is version-locked to the app bundle it
lives in; the Homebrew symlink and the bundle path resolve to the same binary (both print `4.7.5`).

### Minimum supported versions

| Floor | Value | Basis |
|---|---|---|
| macOS | **11.3** | `LSMinimumSystemVersion` in UTM 4.7.5's `Info.plist`; the cask declares `Required: macOS >= 11`. **Unverified** below 26.6.2 — this host is the only one exercised. |
| UTM (hard floor) | **4.2** | UTM 4.1 release notes: "AppleScript (OSA) support and CLI interface". UTM 4.2 release notes: "The scripting interface now allows for creation of a new VM, configuration an existing VM, changing drive image". `profile create` needs both, so 4.1 is not enough. |
| UTM (recommended floor) | **4.3** | UTM 4.3 release notes add "TPM 2.0 and Secure Boot … required for Windows 11 updates to 22H2". Windows 11 needs it (§6). |
| UTM (verified) | **4.7.5** only | everything in this document |

**Decision: `doctor` requires UTM ≥ 4.3.** 4.2 is the point below which nothing works at all, but
Windows 11 is a release requirement (spec §1) and needs the TPM 2.0 / Secure Boot toggles that
arrived in 4.3, so 4.3 is the single number to check. Report anything below 4.3 as unsupported and
anything other than 4.7.5 as untested.

Every row in that table is **unverified except the 4.7.5 row**: the floors are read off UTM's own
release notes and `Info.plist`, not reproduced. Only UTM 4.7.5 on macOS 26.6.2 was executed.

### `utmctl` discovery (FR-6)

The spec's order is config path → `PATH` → `/Applications/UTM.app/Contents/MacOS/utmctl`. Both of
the last two work on this host and are the same file:

```
$ type -a utmctl
utmctl is /opt/homebrew/bin/utmctl
$ /Applications/UTM.app/Contents/MacOS/utmctl version
4.7.5
```

**Verified:** paths containing spaces are fine as long as they are passed as a single argv element
— the ISO used throughout lives at `~/Library/Application Support/wargame-vm/iso/…` and appears
verbatim in the generated QEMU command line
(`file.filename=/Users/moreal/Library/Application Support/wargame-vm/iso/kali-linux-2026.2-installer-arm64.iso`).
Keep using `ProcessRunner` argv arrays; never build a shell string.

### Automation permission (macOS Apple Events / TCC)

- **Verified:** `utmctl` needed no permission prompt at all on this host. It ships *inside*
  `UTM.app`, so its Apple Events to `com.utmapp.UTM` are same-app traffic.
- **Verified:** `osascript` (an out-of-process client) also worked with no prompt in this session.
  The responsible process is the terminal that hosts it (`Ghostty.app` here), which already held
  the Automation → UTM grant. The grant is per *responsible app*, so a user running `wargame` from
  Terminal.app, iTerm or an IDE gets a separate first-run prompt: **"…would like to control UTM"**.
- **Verified (shape):** a *denied* grant produces AppleScript error **-1743**, e.g.
  `execution error: Not authorized to send Apple events to System Events. (-1743)` — captured
  against `System Events`, which this host has not granted (fixture
  `applescript-error-appleevents-denied.txt`). **Unverified:** the exact wording UTM produces when
  its own grant is missing; treat `-1743` (and `errAEEventNotPermitted`) as the denial signal.
- **Not required:** Full Disk Access, root, or a sudoers rule. Contrast with the existing Tart
  `isolated` path, which needs `NOPASSWD` for `softnet` (`docs/verification.md`). UTM holds
  `com.apple.vm.networking` and `com.apple.security.virtualization` itself
  (fixture `utm-4.7.5-entitlements.plist`), so **`host`/`shared`/`bridged` networking needs no
  privilege escalation**. This is a real improvement over the Tart backend and `doctor` should say
  so.
- **Blocked (host, not UTM):** the process cannot read `~/Downloads`
  (`Operation not permitted`) nor `~/Library/Containers/com.utmapp.UTM/Data/Documents`
  (also `Operation not permitted`, the directory exists and is `drwx------`). UTM is sandboxed
  (`com.apple.security.app-sandbox`), so **the adapter must not try to inspect `.utm` bundles on
  disk** — every read has to go through the scripting API.

### Matching on UTM's error output

UTM and `osascript` emit **typographic** apostrophes and quotes (U+2019 `’`, U+201C/D `“ ”`) —
`The operation couldn’t be completed`, `Can’t make … into type text`. The committed fixtures keep
them byte-exact; this document straightens them for readability, so **never copy prose out of this
document into a matcher**.

Match on the stable parts instead, in this order:

1. the numeric code — `-1700`, `-1712`, `-1728`, `-1743`, `-2700`, `-609`, `-10004`;
2. the ASCII-only prefix — `Error from event:` (Apple Event failure, `utmctl` exit 0) or
   `Error:` (`utmctl`'s own failure, exit 1);
3. the ASCII-only second line where UTM supplies one, e.g. `The virtual machine is not running.`,
   `Virtual machine not found.`, `The QEMU guest agent is not running or not installed on the guest.`

Never match a sentence containing an apostrophe.

## 2. Operation matrix

`AS` = AppleScript via `osascript`. Every "verified" row was run against an app-owned
`wargame-t1-*` VM on this host.

| Operation | Mechanism | Exact command | Result |
|---|---|---|---|
| App version | both | `utmctl version` / `tell application "UTM" to return UTM version` | **verified**, `4.7.5` |
| List VMs | both | `utmctl list` (UUID / Status / Name columns) | **verified** |
| Create VM | **AS only** | `make new virtual machine with properties {backend:qemu, configuration:{name:…, architecture:"aarch64"\|"x86_64", memory:…, cpu cores:…, hypervisor:…, uefi:true, drives:{{removable:true, source:isoFile}, {guest size:20480}}}}` | **verified**, returns a specifier (id + name) |
| Attach ISO at create | **AS only** | `drives:{{removable:true, source:isoFile}}` | **verified**; `host size` of the drive comes back as the ISO size in MiB (3788 / 4580) |
| Attach a *second* ISO | **AS only** | `set drives of c to (drives of c) & {{removable:true, source:isoFile}}` + `update configuration` | **verified**, transcript §2 — a third drive appears with `hostMiB=4580` (the amd64 ISO). Needed for the Windows guest-tools CD |
| Replace an ISO | **AS only** | `set item 1 of drives of c to {id:i, source:isoFile}` + `update configuration` | **verified**, transcript §3 — drive 1's `hostMiB` goes 3788 → 4580 after pointing it at the other ISO |
| **Detach** a drive (eject the installer CD) | **AS only** | build a list of the drives to keep as `{id:(id of d)}`, `set drives of c to <kept>` + `update configuration` | **verified on a real installed guest** (`.superpowers/sdd/PLAN/task-7-phase-a-report.md` §3): drives went 2 → 1 and the readback showed only `id=ED07E2A8-… removable=false iface=VirtIO hostMiB=2379`. There is no "remove drive" verb; the whole list is written back. A kept drive **must** echo its own `id`, or UTM adds a drive instead of keeping one. VM must be `stopped` (T9) |
| Change any setting | **AS only** | `update configuration of vm with c` | **verified**; **VM must be `stopped`**, else `-2700 The virtual machine must be stopped before this operation can be performed` |
| Add / remove display | **AS only** | `set displays of c to {{hardware:"virtio-ramfb"}}` / `set displays of c to {}` | **verified** |
| Set network mode | **AS only** | `set item 1 of network interfaces of c to {index:n, mode:host}` | **verified** for all four modes (§5) |
| Set port forwards | **AS only** | `port forwards:{{host port:22022, guest port:22}}` on `mode:emulated` | **verified**; see the `protocol:TCP` bug in §4 |
| Query id / name / backend / status | both | `id of vm`, `name of vm`, `backend of vm`, `status of vm`; `utmctl status <id\|name>` | **verified** |
| Read configuration | **AS only** | `set c to configuration of vm` | **verified**, but incomplete — see §4 |
| Clone | both | `duplicate (first virtual machine whose id is vmId) with properties {configuration:{name:…}}` / `utmctl clone <id> --name <n>` | **verified**; **neither returns the new VM**. The name must be one **no registered VM already carries** — see T14. The loop reference (`duplicate vm`) fails with -10014 |
| Set a VM's MAC address | **AS only** | `set item 1 of network interfaces of c to {index:n, address:"02:AB:CD:12:34:56"}` + `update configuration` | **verified 2026-09-09** (§4 T15): the readback goes `86:61:47:CB:D0:E9` → `02:AB:CD:12:34:56` and the interface's `mode`, `hardware` and port forwards are untouched. A copy inherits its source's address, so every clone needs this |
| Rename a VM | **AS only** | `set name of c to …` + `update configuration` | **verified 2026-09-09**: the **bundle on disk is renamed with it** (`Documents/<old>.utm` → `Documents/<new>.utm`, read out of the running QEMU argv). This is why the name is a scarce resource — T14 |
| Start | both | `utmctl start <id>` / `start vm` | **verified**; `started` in ~2 s (arm64) / ~6 s (x86_64 TCG) |
| Start disposable | both | `utmctl start <id> --disposable` / `start vm without saving` | **unverified** (flag exists in `--help` and the sdef) |
| Start recovery | both | `utmctl start <id> --recovery` / `start vm with recovery` | **unverified** |
| Suspend (pause) | both | `utmctl suspend <id>` / `suspend vm` | **verified**, transcript §10 → `paused` |
| Suspend + save state | both | `utmctl suspend <id> --save-state` | **verified**, transcript §10 → also `paused`, no distinct status |
| Resume | both | `utmctl start <id>` / `start vm` | **verified**, transcript §10, `paused` → `started` |
| Stop (ACPI/force) | both | `utmctl stop <id>` / `stop vm by force` | **verified** for arm64 `virt` (1 s); **hung in `stopping` > 60 s** for x86_64 `q35` sitting in GRUB |
| Stop (kill) | both | `utmctl stop <id> --kill` / `stop vm by kill` | **verified**, `stopped` reported immediately — but see **T12**: `stopped` does not mean the QEMU process is gone, and once it was still alive minutes later |
| Stop (guest request) | both | `utmctl stop <id> --request` | **unverified** (needs an installed OS) |
| Delete | both | `utmctl delete <id>` / `delete (first virtual machine whose id is vmId)` | **verified**, no confirmation. **`delete vm` on a `repeat` loop's reference deletes the whole library** — T13. `utmctl delete <id>` deletes exactly one (**verified 2026-09-09**) |
| Export bundle | **AS only** | `export virtual machine named … to POSIX file "…/x.utm"` | **verified**, transcript §11 — 964 KB for a VM whose disk is an untouched sparse qcow2 |
| Import bundle | **AS only** | `import new virtual machine from POSIX file "…/x.utm"` | **verified**, transcript §11 — returns a specifier, but **keeps the source display name**, so `utmctl list` then showed two VMs called `wargame-t1-evidence` |
| Serial console address | **AS only** | `address of first serial port of vm` | **verified**: a `/dev/ttysNNN` path while running, **empty string while stopped**. The number is allocated per run — `ttys010`, `ttys002` and `ttys003` all appear across the captures — so never cache it across a stop |
| Serial console attach | — | `utmctl attach <id>`, `utmctl start <id> --attach` | **verified BROKEN**, `attach-and-default-drives.txt` §4–§5: both print `WARNING: attach command is not implemented yet!` on **stdout**, exit 0, and stream nothing; `attach` adds the ptty path, `start --attach` still starts the VM |
| Graphical console | app UI | display device configured → UTM opens a window named after the VM | **verified** (§3) |
| Guest IP | both | `utmctl ip-address <id>` / `query ip of vm` | **verified failure** pre-install: `The QEMU guest agent is not running or not installed on the guest` |
| Guest exec | both | `utmctl exec <id> --cmd …` / `execute of vm at …` | **verified failure** pre-install, same reason |
| Guest file push/pull | both | `utmctl file push\|pull` / `open file of vm at …` | **unverified** (needs the guest agent) |
| USB attach | both | `utmctl usb …` / `connect device to vm` | **unverified**, not needed |
| Input automation | **AS only** | `input scan code of vm codes {28, 156}`, `input keystroke of vm text "…"` | **unverified / unreliable — do not build on it.** Both return exit 0 and reach no guest on this host; see the note below the table |
| Hide the UTM window | — | `utmctl … --hide` | **verified BROKEN**: emits `OSStatus error -10004` twice; see §4 |

#### Input automation: exit 0 and nothing happens (downgraded 2026-09-09)

Task 1 marked `input scan code` / `input keystroke` **verified** on the strength of "exit 0, and
GRUB redrew around the same time". Task 7 phase A tried to *drive an installer* with them, against
two real VMs, and got nothing at all — every attempt returned success and produced **zero** bytes
of guest reaction on a serial console that was simultaneously showing live GRUB output, so the
guest was awake and the console worked; only the injected input vanished
(`.superpowers/sdd/PLAN/task-7-phase-a-report.md` §5 D6):

| attempt | result |
|---|---|
| `input keystroke <vm> text "c"`, no display device | `typed 1 chars`, 0 bytes of guest reaction |
| `input keystroke <vm> text "e"`, `virtio-ramfb` display, VM window open | `typed 1 chars`, 0 bytes |
| same, after `tell application "UTM" to activate` | `typed 1 chars`, 0 bytes |
| `input scan code <vm> codes {28,156}` (Enter) | `sent 2 codes`, 0 bytes |
| `input scan code <vm> codes {224,80,224,208}` (Down) | exit 0, 0 bytes |

The earlier "GRUB redrew" observation is now explained by §3's echo trap: the serial ptty was
echoing GRUB's own output back at it. Treat this row as **unusable**. Anything that would depend on
it — a scripted Windows install, unattended menu navigation — needs another mechanism (an
unattended-install file baked into or served to the installer is what phase A used).

**Trap for whoever writes the helper anyway:** an AppleScript local named `codes` collides with the
command's own parameter name and fails to compile with `-2741 Expected expression but found
parameter name`. Name the variable anything else. (The same class of collision is why the eject
script in `wargame-utm` uses `kept`/`gone` rather than anything from the dictionary.)

### State machine: the `status` and `backend` enumerations

Quoted from `utm-4.7.5.sdef`. These are the complete sets — the adapter's state machine can be
written from this table alone.

`status` (enumeration `VmEs`), the value returned by `status of vm` and printed by `utmctl status`:

| Value | sdef description | Observed on this host |
|---|---|---|
| `stopped` | "VM is not running." | yes — the resting state; **the only state in which `update configuration` is accepted** |
| `starting` | "VM is starting up." | not observed (start reached `started` within the 1 s polling interval) |
| `started` | "VM is running." | yes |
| `pausing` | "VM is going to pause." | not observed (transient) |
| `paused` | "VM is paused." | yes — after `suspend`, with or without `--save-state`. **`stop` refuses a paused VM** ("The virtual machine is not running", exit 0); resume it with `start` first |
| `resuming` | "VM is resuming from pause." | not observed (transient) |
| `stopping` | "VM is stopping." | yes — and it can persist (>60 s for an x86_64 TCG VM at the GRUB prompt). **`update configuration` fails here with -2700**, so poll for `stopped`, never for "not `started`" |

Transitions verified end to end in `readback-transcript.txt` §10:
`stopped --start--> started --suspend--> paused --start--> started --suspend --save-state--> paused
--start--> started --stop--> stopped`. There is no separate "saved" status: `suspend --save-state`
also lands in `paused`.

`backend` (enumeration `VmEb`), the value returned by `backend of vm`:

| Value | sdef description |
|---|---|
| `apple` | "Apple Virtualization.framework backend." |
| `qemu` | "QEMU backend." — the only one this project uses |
| `unavailable` | "The virtual machine is not currently available." |

**`unavailable` matters.** It is not an error; it is a third value a registered VM can report, and
the dictionary gives no way to ask *why*. Treat it as "this VM exists in UTM's registry but its
backend cannot be used right now" — map it to the spec's `Missing`/unusable state, never delete on
it, and re-read before acting. It was **not** reproduced on this host, so its exact trigger is
**unverified**.

Two related lifecycle rules, both verified:

- `status` is not enough on its own after a stop — see trap T12, where the QEMU process outlived a
  `stop --kill` while `utmctl status` already said `stopped`.
- `stop` and `start` are Apple Events with the exit-code defect from T1; always re-read `status`
  rather than trusting the verb's return.

### What is AppleScript-only

`utmctl` exposes **no** way to create a VM, read or write a configuration, attach or swap an ISO,
change CPU/memory/network/display, or import/export. Those are all `make` / `duplicate` /
`configuration` / `update configuration` / `import` / `export` in the dictionary. The adapter needs
an `osascript` path regardless; `utmctl` is only a convenience for list/status/start/stop/clone/
delete plus the guest-agent verbs. Since `utmctl` is itself "a wrapper around the AppleScript
interface" (UTM docs) and has the exit-code defects in §4, **prefer `osascript` with a trusted
script template and arguments passed via `on run argv`** — which is also what spec §5 item 6 asks
for.

## 3. Console behaviour

- **Verified:** a VM created through `make` has **no display device** (`displays:{}`), one `ptty`
  serial port and one `shared` network interface. This matches the cheat sheet's note. Consequence
  for FR-2: `profile create` **must add a display device** or the user can never do the manual OS
  install the spec allows.
- **Verified:** with `displays:{}`, firmware/GRUB output goes to the serial ptty and nothing else.
  `utmctl start` launches/foregrounds UTM and only its main window exists:
  `tell application "UTM" to return name of every window` → `UTM`.
- **Verified:** with a display configured, the same query returns
  `wargame-t1-kali-arm64, UTM` — UTM opens a **second window titled with the VM name**, and the
  serial ptty then stays silent. So "open the console" = have a display device and activate the
  app (`open -a UTM`); there is no `utmctl console` verb.
- **Verified:** `utmctl start` therefore *does* show a window. There is no headless start:
  `--hide` (whose help says "Hide the main UTM window") is broken (§4).
- **Verified:** the serial ptty is the machine-readable console. Get it from
  `address of first serial port of vm` **after** the VM reports `started` (it is `""` while
  stopped) and read the device directly.
- **Verified:** there is no working attach verb, and it does not block — it is a no-op with a
  warning. Captured in `attach-and-default-drives.txt` with stdout and stderr kept apart and the
  un-piped exit status recorded:

  ```
  ## 4  $ utmctl attach wargame-t1-attach2 </dev/null          # VM already running
  exit=0   elapsed=0s
  --- stdout (67 bytes) ---
  WARNING: attach command is not implemented yet!
  PTTY: /dev/ttys003
  --- stderr (0 bytes) ---

  ## 5  $ utmctl start wargame-t1-attach2 --attach </dev/null  # VM stopped, 3 consecutive runs
  exit=0   elapsed=2s        (run 1, run 2 and run 3 are identical)
  --- stdout (48 bytes) ---
  WARNING: attach command is not implemented yet!
  --- stderr (0 bytes) ---
  $ utmctl status wargame-t1-attach2
  started
  ```

  So `start --attach` is **a normal start with a misleading warning**: it returns in ~2 s, exits 0,
  writes only that one line to stdout, and the VM is `started`. It never streams serial data.
  Use plain `start` and read the ptty yourself.
- **Anomaly, recorded not resolved:** two earlier `timeout 15 utmctl start <vm> --attach` runs
  against a *different* VM instead ran the full 15 s and were killed by the timeout (exit 124)
  having written nothing to either stream. Both happened while that VM had the orphaned QEMU
  process of T12 holding its `efi_vars.fd`, i.e. in the wedged-app situation of T8. The three
  clean runs above did not reproduce it. Treat a blocked `--attach` as a symptom of T8/T12, and
  put a timeout on the call regardless.
- **Verified:** the `serial ports` list of a *configuration record* reads back empty (T5), so the
  ptty path is only reachable through the VM element, never through the configuration.

### Trap: the ptty echoes the guest back at itself, and the fix has an ordering rule

**Verified in task 7 phase A** (`.superpowers/sdd/PLAN/task-7-phase-a-report.md` §3), and the
single largest cost of that run. The ptty comes up in the default line discipline — `icanon`,
**`echo`** — so every byte QEMU writes to the master is echoed straight back as serial *input*. The
guest keyboard-mashes itself: GRUB's own menu redraw pressed Enter and booted the default entry
unprompted, and later GRUB's countdown never expired, because "input available" cancels it forever.
This is also what made task 1's `input scan code` look like it worked.

Turning echo off has an ordering trap of its own: **opening a pty slave that nobody currently holds
resets its termios**, so an `stty -f "$SER" raw -echo` issued *before* the reader starts is undone
the moment `cat` opens the device. The reader has to open it first:

```bash
utmctl start "$UUID"
SER=$(osascript utm-serial.applescript "$UUID")   # empty while stopped; re-read after every start
cat "$SER" >> serial.log &                        # 1. reader opens the ptty
sleep 1
stty -f "$SER" raw -echo                          # 2. only now is echo actually off
```

With that in place the ptty is a fully bidirectional console: `printf 'c' > $SER` reaches GRUB, and
the whole Debian installer is drivable over it. §8's read-only `timeout 25 cat "$SER"` reproduction
is unaffected — the trap only bites when something writes to the device as well.

### Trap: the stock Kali **amd64** ISO's GRUB loses serial **input** as well as output

**Verified in task 7 phase A2** (`.superpowers/sdd/PLAN/task-7-phase-a2-report.md` §2). The arm64
ISO's GRUB talks to the serial ptty and can be driven from it (§7, and the boot line quoted in that
report). The amd64 ISO's cannot, and the reason is in its own `/boot/grub/grub.cfg`, which opens
with `loadfont` / `insmod gfxterm` / `terminal_output gfxterm` — that takes GRUB off the serial
line.

Phase A recorded only the visible half: **output** disappears, so the menu never reaches the ptty.
Phase A2 tested the other half deliberately. Two blind writes — `c`, then
`terminal_output console` — at t+175 s and t+319 s after power-on, with QEMU at 195 % CPU so GRUB
was demonstrably alive, produced **0 bytes** of response each time. So **input is lost too**: there
is no "type blind and recover the console" fallback.

- **Verified:** with no other input channel, the stock amd64 installer **cannot be agent-driven at
  all.** The other two channels are gone as well — UTM's `input scan code` / `input keystroke` are
  exit-0 no-ops on this host (`task-7-phase-a-report.md` D6; the ptty echo trap above is what made
  them look like they worked in task 1), and `screencapture` needs a Screen Recording grant an agent
  does not have. A human at the UTM window is unaffected; this is a statement about automation only.
- **Verified:** the working route is a **modified ISO**, not a convenience. Phase A2 built a scratch
  copy whose only change was a `grub.cfg` without the `gfxterm` preamble, `set timeout=5` and one
  unattended entry:

  ```bash
  nix shell nixpkgs#xorriso -c xorriso \
    -indev  .../kali-linux-2026.2-installer-amd64.iso \
    -outdev .../kali-t7-amd64.iso \
    -boot_image any replay -overwrite on \
    -map grub-amd64.cfg /boot/grub/grub.cfg -commit -eject all
  ```

  The pristine ISO was never modified (its SHA-256 was re-verified unchanged afterwards), and the
  scratch copy was removed from the VM by `wargame profile eject`. Editing the boot line by hand at
  the graphical GRUB prompt is the same fix without the rebuild.
- **Unverified:** whether a serial-friendly amd64 ISO exists upstream, and whether UEFI firmware
  settings can force GRUB back onto the serial console before `grub.cfg` runs. Neither was tried.
- **Consequence:** any future scripted install on a **non-arm64** guest — a Windows one included —
  has to plan for "the boot loader may be unreachable over the serial line in both directions", and
  budget either a prepared boot medium or a human at the console. Do not design a flow that assumes
  blind keystrokes land.

### Two more things only an installed guest could show

- **Verified:** `utmctl ip-address <id>` on an installed guest answers with **three lines** — IPv4,
  a global (ULA) IPv6 and a link-local `fe80::` — not one
  (fixture `utmctl-ip-address-three-lines.txt`). A caller must select by kind (IPv4, else a
  routable address); `lines[0]` is right by luck and wrong the moment the agent reorders them.
- **Verified (Kali):** `systemctl is-enabled qemu-guest-agent` answers **`static`**, not `enabled`
  — the unit has no `[Install]` section and is activated by udev. `is-active` is `active`. A
  readiness check or a piece of guidance that insists on the literal string `enabled` calls every
  correct Kali guest broken.

## 4. Defects and traps found (with evidence)

Fifteen of them. Each changes what a `wargame-utm` adapter has to do. Raw output for every one is
in `crates/wargame-utm/tests/fixtures/`; the readback-heavy ones cite
`readback-transcript.txt` by section. T13–T15 were found by the real-VM acceptance run
(`.superpowers/sdd/PLAN/task-7-b2b-report.md`) and confirmed on throwaway VMs afterwards.

### T1. `utmctl` exit codes do not report Apple-Event failures

```
$ utmctl stop wargame-t1-kali-arm64 1>/dev/null      # VM already stopped
Error from event: The operation couldn't be completed. (OSStatus error -2700.)
The virtual machine is not running.
$ echo $?
0
```

Same for `ip-address`/`exec` without a guest agent, and for a `start` that fails with `-1712`
(timeout) or `-609` (connection invalid). Only `Error: Virtual machine not found.` exits **1**.

*Resolution:* never trust `utmctl`'s exit code alone. Capture stderr, treat any
`Error from event:` / `Error:` line as failure, and re-check with `utmctl status`. Better: use
`osascript`, which does propagate a non-zero exit for these.

### T2. `duplicate` and `utmctl clone` return nothing

```
$ osascript -e 'tell application "UTM"
	set dup to duplicate (virtual machine named "wargame-t1-x86") with properties {configuration:{name:"wargame-t1-x86-dup"}}
	return id of dup
end tell'
execution error: The variable dup is not defined. (-2753)
```

The VM *was* created. The sdef's `duplicate` command has no `<result>` element (unlike `make`,
which does). `utmctl clone` likewise prints nothing on success.

*Resolution:* clone with a caller-chosen unique name, then resolve the UUID with
`id of virtual machine named <name>`. Without `with properties` the copy is named `"<source> 2"`
(**verified**), which is not something to rely on.

### T3. VM display names are not unique, and name lookup silently picks one

`import` of an exported bundle produced a second VM with the *same* name; `utmctl list` showed both
`AE4AC721-…` and `12261215-…` as `wargame-t1 한글 이름 vm`, and
`id of virtual machine named "…"` returned the first without error.

*Resolution:* exactly what FR-5 already requires — store the **UUID** as the provider id and only
ever use names for display. `utmctl status <prefix>` correctly refuses a partial name
(`Error: Virtual machine not found.`), so full-name matching is not a substitute.

### T4. `protocol:TCP` in a port forward is rejected

```
… port forwards:{{protocol:TCP, host port:22022, guest port:22}} …
execution error: UTM got an error: Can't make {… port forwards:{{protocol:tcp, …}}} …
into type qemu configuration or apple configuration. (-1700)
```

Note the lower-cased `tcp` in the echoed record. The dictionary defines **two** terminology names
that differ only in case: `network protocol` → `TCP` (code `TcPp`) and `serial interface` → `tcp`
(code `TcP `). AppleScript resolves the literal to the *serial interface* enumerator, which UTM
then refuses. `protocol:UDP` (code `UdPp`, no collision) works as written — **verified**.

*Resolution (both verified):* omit `protocol`, which defaults to TCP, or write the raw code
`protocol:«constant ****TcPp»`. Fixture
`applescript/set-port-forward-raw-tcp-code.applescript` uses the latter.

### T5. The configuration record is not round-trippable

Reading `drives of configuration of vm` returns only `id`, `removable`, `interface` and
`host size`. `source` and `guest size` are **write-only in practice**: asking for them inside the
returned record fails with `-1700` (`Can't make «class GuSz» of item 1 of «class DrVs» … into type
text`).

*Resolution:* the adapter cannot ask UTM which ISO is attached or how big the guest disk is. Record
the ISO path and the requested disk size in the app's own build record (spec §5 item 5) and treat
UTM as write-mostly. `host size` (MiB, host-side allocation) is the only usable cross-check: it
equals the ISO size for a removable drive and `0` for a fresh sparse qcow2.

Three related quirks, all in `readback-transcript.txt` §4:

- `count of <any list property>` returns **0**. Use `length of (…)` or a `repeat` loop.
- **`serial ports` of a configuration record is unreachable as a list** — loop, `count of` and
  `length of` all give 0, even though the flattened whole-record dump plainly contains
  `serial ports:port:0, index:0, interface:ptty, hardware:`. `drives`, `displays`,
  `network interfaces` and `qemu additional arguments` all behave normally. To reach the serial
  console use the VM element (`address of first serial port of vm`), which works, not the
  configuration record.

  ```
  drives: loop=3 count-of=0 length-of=3  ||  serial ports: loop=0 count-of=0 length-of=0
  ```
- `POSIX file <variable>` must be evaluated **outside** the `tell application "UTM"` block,
  otherwise UTM resolves the path in its own sandbox and fails with `-1728 Can't get POSIX file "…"`.

One more shape worth knowing: **`make` with the `drives` key omitted entirely still produces two
drives** — a removable CD with no image plus a blank qcow2. Captured live in
`attach-and-default-drives.txt` §2–§3 for a VM created with only `{name, architecture, memory,
cpu cores}`:

```
$ osascript applescript/show-drives.applescript wargame-t1-attach2
drives (2):
  [1] id=73DA22FF-B99C-4842-926E-45CD98F102A0 removable=true iface=USB hostMiB=0
  [2] id=ACB23102-DE86-41CB-BEE3-F0FCA3F30482 removable=false iface=VirtIO hostMiB=0

$ ps -Aww -o command | grep qemu-aarch64-softmmu | grep -v grep | tr ' ' '\n' \
    | grep -E 'media=cdrom|media=disk|usb-storage|virtio-blk'
usb-storage,drive=drive73DA22FF-…,removable=true,bootindex=0,bus=usb-bus.0
if=none,media=cdrom,id=drive73DA22FF-…,readonly=on
virtio-blk-pci,drive=driveACB23102-…,serial=ACB23102DE8641CBBEE3,bootindex=1
if=none,media=disk,id=driveACB23102-…,file.filename=…/wargame-t1-attach2.utm/Data/ACB23102-….qcow2
```

Note the CD has **no `file.filename`** at all. Always pass `drives` explicitly rather than
inheriting this.

### T6. `hypervisor` is silently ignored for a non-matching architecture

The x86_64 VM was created with `hypervisor:false`, started, stopped, reconfigured to
`hypervisor:true`, and started again. The configuration readback changes; the accelerator does not
(`readback-transcript.txt` §12):

```
$ osascript … 'return "config readback hypervisor=" & ((hypervisor of c) as text)'
config readback hypervisor=false
$ ps -Aww -o command | grep qemu-x86_64-softmmu | grep -v grep | grep -oE '\-accel [^ ]+|\-smp [^ ]+|\-cpu [^ ]+'
-cpu Skylake-Client
-smp cpus=4,sockets=1,cores=4,threads=1
-accel tcg,tb-size=1024
$ osascript … 'set hypervisor of c to true; update configuration of vm with c; return "update issued"'
update issued
$ osascript … 'return "config readback hypervisor=" & ((hypervisor of c) as text)'
config readback hypervisor=true
$ ps -Aww -o command | grep qemu-x86_64-softmmu | grep -v grep | grep -oE '\-accel [^ ]+|\-smp [^ ]+|\-cpu [^ ]+'
-cpu Skylake-Client
-smp cpus=4,sockets=1,cores=4,threads=1
-accel tcg,tb-size=1024
```

(The two committed argv fixtures for this VM, `qemu-argv-x86_64-emulate-hypervisor-off.txt` and
`…-on-ignored.txt`, are byte-identical *because that is the finding* — the configuration flip is
invisible in the command line. The readback above is what distinguishes the two runs.)
The official docs say the same: "If the guest architecture does not match the host, virtualization
will not be used even if enabled in the QEMU settings."

*Resolution:* FR-1 forbids silently changing mode, and UTM will not tell us it downgraded. Our
compat layer must reject `mode: virtualize` for `x86_64` on an Apple Silicon host **before**
creating anything, and set `hypervisor:false` explicitly for every emulated VM.

### T7. `--hide` is broken

```
$ utmctl start --hide wargame-t1-kali-arm64
Error from event: The operation couldn't be completed. (OSStatus error -10004.)
Error from event: The operation couldn't be completed. (OSStatus error -10004.)
$ echo $?; utmctl status wargame-t1-kali-arm64
0
started
```

`-10004` is `errAEPrivilegeError`. The VM did start; only the hide-window event failed. In one
earlier attempt the same invocation ended with `-1712` (timeout) after 120 s and the VM did **not**
start.

*Resolution:* never pass `--hide`. There is no supported headless start; the UTM window is part of
the deal (which is fine — spec §4 wants a console anyway).

### T8. `utmctl`/`osascript` calls fail if the app is not already up

Two reproducible shapes, both with **exit 0**:

- `-609` (`connectionInvalid`) when the call lands while UTM is not running or is still launching.
- `-1712` (`errAETimeout`) after ~120 s when the app is up but wedged.

Every subsequent call in this session succeeded after `open -a UTM`, a short wait, and a
`utmctl version` handshake.

*Resolution:* the adapter should ensure the app is running (`open -a UTM`, poll `utmctl version`
until it prints a version, bounded) before any real command, and retry once on `-609`/`-1712`.
`doctor` should perform that handshake and report it.

### T9. `stop` can hang; `update configuration` needs a fully stopped VM

`utmctl stop` returns immediately but the VM passes through `stopping`. For the x86_64 TCG VM
sitting at the GRUB prompt, `stopping` persisted past 60 s (`utmctl stop` sends an ACPI power-off
that nothing was listening for); `utmctl stop --kill` then reported `stopped` at once and the QEMU
process was gone within seconds. Meanwhile `update configuration` on a `stopping` VM fails with
`-2700 The virtual machine must be stopped before this operation can be performed`.

*Resolution:* separate stop timeouts as FR-5 requires, poll `utmctl status` until `stopped` (not
just `stop`'s return), escalate `force → kill` on timeout, and gate every configuration write on
an observed `stopped`. But `stopped` is still not sufficient — see T12.

### T10. `qemu additional arguments` is a real escape hatch

`set qemu additional arguments of c to {{argument string:"-device"}, {argument string:"virtio-balloon-pci"}}`
round-tripped (**verified**, `readback-transcript.txt` §5 — `readback: -device virtio-balloon-pci`,
then setting it back to `{}` gives `cleared, length=0`). Each argv element is its own record.
Reading it back needs `set c2 to configuration of vm` first; the inline
`qemu additional arguments of (configuration of vm)` form fails with `-1700`. This is the only scriptable way
to reach QEMU features UTM's dictionary omits — but see §6: it is *not* a usable path to TPM.

### T11. Silent fallback on an unknown display hardware name

Setting `displays` to `{{hardware:"definitely-not-a-card"}}` returned exit 0 and left
`hardware=virtio-ramfb`. The sdef documents this ("if given hardware not found, the default will be
used"), so an invalid value is not reported.

*Resolution:* validate display/network hardware names in our layer; do not expect UTM to complain.

### T12. `stop --kill` can leave an orphan QEMU process that UTM reports as stopped

**Observed once, on 2026-09-09, on a small aarch64 VM; not reproduced deliberately since — the
trigger is unknown.** Full evidence in `orphan-qemu-process.txt`, whose header records that it is a
one-off transcription of individual commands rather than a scripted run; the stderr block is also
committed on its own as `utmctl-error-orphan-qemu-lock.stderr.txt`, captured live with `2> <file>`.

After `utmctl stop … --kill`, `utmctl status` said `stopped` **within 1 s while the QEMU process
was still alive** — parented to UTM's `QEMUHelper` XPC service, in state `R`, still there minutes
later:

```
$ utmctl stop wargame-t1-attach --kill
exit=0
$ utmctl status wargame-t1-attach
stopped
$ ps -o pid,ppid,lstart,stat,etime,command -p 58445
  PID  PPID STARTED                      STAT ELAPSED COMMAND
58445 92742 Wed Sep  9 02:27:32 2026     R      03:07 /Applications/UTM.app/Contents/XPCServices/…/QEMULauncher …/qemu
$ ps -o pid,command -p 92742
  PID COMMAND
92742 /Applications/UTM.app/Contents/XPCServices/QEMUHelper.xpc/Contents/MacOS/QEMUHelper
```

Every subsequent start of that VM then failed on the firmware variable store, and (per T1) still
exited 0:

```
$ utmctl start wargame-t1-attach                        # stderr; stdout empty; exit 0
Error from event: The operation couldn't be completed. (OSStatus error -2700.)
QEMU error: QEMU exited from an error: Is another process using the image
[/Users/moreal/Library/Containers/com.utmapp.UTM/Data/Documents/wargame-t1-attach.utm/Data/efi_vars.fd]?
$ utmctl status wargame-t1-attach
stopped
```

Waiting did not help — identical output after 0 s, 3 s and 10 s of extra wait, with `pgrep`
returning the same pid each time. Recovery was `kill -9 58445`, after which `utmctl start`
succeeded and `status` went to `started`.

This is the mirror image of the existing Tart bug 13 in `docs/verification.md`: there the record
said `running` while the VM had stopped; here UTM says `stopped` while the backend is still up.

*Resolution:* the adapter must not treat `status == stopped` as "the VM's files are free". After a
stop — and before any restart, clone, `update configuration` or delete — confirm that no
`qemu-*-softmmu` process carries this VM's `-uuid`. `ps -Aww -o command` shows the full argv, and
QEMU's `-uuid` equals UTM's VM id exactly, so ownership is unambiguous and no name matching is
needed (`-name` is the display name with non-alphanumerics stripped, and is not a reliable key).

**On detection, fail the stop; do not kill.** Report the stop as incomplete, naming the orphan pid
and quoting the `Is another process using the image` line verbatim, and tell the user that
`kill -9 <pid>` clears it. `wargame` must not send signals to a process it did not spawn: the
process belongs to UTM's `QEMUHelper`, the user may be looking at that VM in the UTM window, and
FR-5 already forbids acting on VMs we do not own. A `--force`-style opt-in that kills a
UUID-matched orphan on the user's explicit instruction is acceptable later; silent `kill -9` is
not. FR-5's "실패 시 단계별 정리와 정리 실패를 기록한다" covers exactly this: record the cleanup
failure, do not paper over it.

### T13. A verb applied to a `repeat` loop's reference acts on the **whole collection**

`repeat with vm in (virtual machines)` binds `vm` to a reference *into* the collection, not to a
copy of the element. `delete` follows that reference back to the collection:

```
--- before ---
8BDD289D-…  stopped  wg-build-t7-p1
5F3D66E8-…  stopped  wg-build-t7-p2      <-- the only id passed to the script
1F4BB274-…  stopped  wg-build-t7-p3
--- run --- osascript delete-exact.applescript 5F3D66E8-84DC-45F3-93DF-986A0473AA74
ok                                        <-- reported success
  osascript exit=0
--- after ---
UUID                                 Status   Name
(empty)                                   <-- all three gone, on disk
```

`start`, `stop` and the `update configuration` writes happened to address the right VM through the
same reference, and `duplicate` refuses it outright with `Handler only handles single objects.
(-10014)` (T2/§2). So the shape is not uniformly wrong — it is *unpredictably* wrong, which is
worse. A running VM raises `-2700` and aborts the cascade partway, so a partial deletion looks like
an ordinary error.

*Resolution:* **never apply a verb to the loop variable.** Use the loop only as the existence check
that produces the not-found sentinel, and address the target as
`(first virtual machine whose id is vmId)` — the single-object specifier — in every verb and every
`update configuration`. `crates/wargame-utm/src/automation.rs` enumerates its own templates in a
test and fails if any of them contains `<verb> vm`.

### T14. A VM's **name is its bundle name**, and `duplicate` refuses a name that is taken

Every VM lives in `~/Library/Containers/com.utmapp.UTM/Data/Documents/<display name>.utm`, and
renaming the VM renames the directory. `duplicate` copies the bundle, registers the copy, applies
the requested name and only then moves the bundle into place — so a name another VM already
carries fails that last step:

```
$ osascript dup.applescript 4E355E9E-… wg-build-image-t7c        # first copy
ok
$ osascript dup.applescript 4E355E9E-… wg-build-image-t7c        # same name again
execution error: UTM got an error: “wg-build-t7c-src 2.utm” couldn’t be moved to “Documents”
because an item with the same name already exists. (-2700)
exit=1
$ utmctl list
4E355E9E-…  stopped  wg-build-t7c-src
9A8BFBFD-…  stopped  wg-build-image-t7c      <-- registered by the FAILED duplicate
A38289F3-…  stopped  wg-build-image-t7c
```

Two things to take from that transcript:

* **The message names the wrong thing.** `wg-build-t7c-src 2.utm` is the *staging* name UTM copies
  under; the name that actually clashed is the one that was asked for. Do not chase the source.
* **A failed `duplicate` still leaves a registered VM.** It is a real, usable VM with the requested
  name; nothing removes it.

A copy under a **free** name always succeeds, however many other VMs exist — verified immediately
after the failure above:

```
$ osascript dup.applescript 4E355E9E-… wg-build-image-t7c-second
ok
$ utmctl list | grep image
B8B5DAEF-…  stopped  wg-build-image-t7c-second
B525AF67-…  stopped  wg-build-image-t7c
```

*Resolution, and what `wargame` does.* Publishing a rebuilt image cannot reuse a fixed name,
because the image being replaced still carries it until the record points at the new one (spec.md
§3 FR-3 requires keeping it that long). So a build publishes under
`wg-build-image-<profile>-<6 hex>` — a name nothing holds — the record names the image by UUID,
and only then is the previous image deleted. The adapter also refuses a taken name itself, before
`duplicate` runs, so the user sees the name that clashed rather than the staging bundle; and every
VM UTM registers during a publish is written into the build record as a leaked image *before* any
error is raised, so a failure can never leave one untracked.

### T15. A copy inherits its source's MAC address

`duplicate` copies the network interface verbatim. Two sessions cloned from one image came up on
the same `bridge105` segment with the same address and the same host key, and an `scp` addressed to
that IP landed in the other guest (`task-7-b2b-report.md` D4):

```
wg-build-image-t7-kali-arm64: mac=02:FD:8F:A5:F2:75
wg-dh-no-main-2f8d45:         mac=02:FD:8F:A5:F2:75   ip=192.168.128.2
wg-fi-hello-fixture-3c715d:   mac=02:FD:8F:A5:F2:75   ip=192.168.128.2
```

A VM made by `make new virtual machine` gets a fresh one, so this is specific to copies.

The sdef's `qemu network configuration` has a writable `address` — "MAC address (formatted as
XX:XX:XX:XX:XX:XX, if empty a random address will be genertaed)" — and a **partial** interface
record updates only what it names (verified 2026-09-09):

```
$ osascript rawnet.applescript B525AF67-…
mode=shared hw=virtio-net-pci mac=86:61:47:CB:D0:E9 idx=0
$ osascript setmac.applescript B525AF67-… 02:AB:CD:12:34:56
ok
$ osascript rawnet.applescript B525AF67-…
mode=shared hw=virtio-net-pci mac=02:AB:CD:12:34:56 idx=0     <-- mode and hardware untouched
```

*Resolution:* write a fresh locally-administered unicast address (first octet `& 0x03 == 0x02`)
onto every copy, while it is still stopped (T9), before anything can start it. Read `index` back
first: the sdef says it "is invalid after updating the configuration and must be reset to the
current position", and the first interface answers **0**.

## 5. Network modes (FR-6)

The dictionary's `qemu network mode` enumeration has exactly four members, quoted verbatim from
`utm-4.7.5.sdef`:

| Mode | sdef description | Generated QEMU netdev (**verified**) |
|---|---|---|
| `shared` | "NAT based sharing with the host." | `-netdev vmnet-shared,id=net0` |
| `host` | "NAT based sharing with no WAN routing." | `-netdev vmnet-host,id=net0` |
| `emulated` | "Emulate a VLAN." | `-netdev user,id=net0,hostfwd=tcp::22022-:22` |
| `bridged` | "Bridged to a host interface." | `-netdev vmnet-bridged,id=net0,ifname=en0` |

All four were set through `update configuration` and started successfully on the arm64 VM; the
argv for each is committed. Additional **verified** facts:

- Default NIC hardware is `virtio-net-pci` on `aarch64`/`virt` and `e1000` on `x86_64`/`q35`. A new
  VM gets exactly one interface in `shared` mode.
- `port forwards` is honoured only in `emulated` mode (the sdef says so, and only that mode's argv
  contains `hostfwd=`). Multiple forwards and `host address` / `guest address` round-trip.
- `host interface` is only meaningful for `bridged`; a stale value left over from a bridged run is
  ignored by the other modes (the `host` and `shared` argv contain no `ifname=`).
- **Verified — the host gets an address on the guest's segment.** With the VM stopped, four stale
  vmnet bridges existed; starting it in `host` mode added `bridge104`, and only then
  (`readback-transcript.txt` §6–§7):

  §6, with the VM stopped — `BRIDGES` below is the awk one-liner
  `ifconfig | awk '/^bridge1[0-9][0-9]:/{b=substr($1,1,length($1)-1)} /inet /{if(b!=""){print b, $2; b=""}}'`:

  ```
  $ osascript applescript/set-network-mode.applescript wargame-t1-evidence host
  mode=host hardware=virtio-net-pci hostif=
  exit=0

  $ utmctl status wargame-t1-evidence
  stopped
  exit=0

  $ BRIDGES
  bridge100 192.168.139.3
  bridge101 192.168.194.0
  bridge102 192.168.215.0
  bridge103 192.168.117.0
  exit=0
  ```

  §7, after starting it — note `bridge104` is the only new line:

  ```
  $ utmctl start wargame-t1-evidence
  exit=0

  $ utmctl status wargame-t1-evidence
  started
  exit=0

  $ ps -Aww -o command | grep qemu-aarch64-softmmu | grep -v grep | grep -oE '\-netdev [^ ]+'
  -netdev vmnet-host,id=net0
  exit=0

  $ BRIDGES
  bridge100 192.168.139.3
  bridge101 192.168.194.0
  bridge102 192.168.215.0
  bridge103 192.168.117.0
  bridge104 192.168.128.1
  exit=0

  $ ifconfig bridge104
  bridge104: flags=8a63<UP,BROADCAST,SMART,RUNNING,ALLMULTI,SIMPLEX,MULTICAST> mtu 1500
  	inet 192.168.128.1 netmask 0xffffff00 broadcast 192.168.128.255
  	member: vmenet4 flags=3<LEARNING,DISCOVER>
  exit=0
  ```

  (`ifconfig bridge104` prints more lines than the three kept here; the full output is in the
  transcript.)

  So **macOS `vmnet` host mode gives the host `.1` on the guest's own segment**, which is the "host
  management connection" half of FR-6. The guest-side half (that the guest cannot reach the LAN or
  the internet) is still **unverified** — see "Which mode is FR-6's `isolated`?" below.
  Note that each mode switch left its previous vmnet bridge behind (`bridge100`–`bridge103` above
  are leftovers), so `doctor` must not treat "a vmnet bridge exists" as evidence of anything.
- No sudo, no sudoers rule, no helper daemon: UTM's own `com.apple.vm.networking` entitlement
  covers `vmnet-shared`, `vmnet-host` and `vmnet-bridged`.

### Which mode is FR-6's `isolated`?

**`host` is the only candidate**, and the mapping is verified at the mechanism level: UTM's `host`
mode becomes `-netdev vmnet-host`, i.e. Apple `vmnet` host mode, the same primitive Tart reaches
through `--net-host`/Softnet in the existing `isolated` path (`DESIGN.md` §1). `emulated`
(`-netdev user`, libslirp) is **not** isolated — SLIRP proxies guest traffic through the host's own
stack and reaches the internet; UTM's own Windows guide even notes libslirp's limitation that
`ping` does not work, which implies the rest does. `shared` and `bridged` are plainly not isolated.

**UNVERIFIED — guest-side isolation.** Whether the guest can be blocked from the LAN and the
internet while the host still reaches it could not be proven on this host in this task, because
there is no installed guest OS yet:

- `utmctl ip-address` / `query ip` require the QEMU guest agent and fail before install
  (**verified**: `The QEMU guest agent is not running or not installed on the guest`).
- The only pre-install code that runs is firmware and GRUB, neither of which configures IP.
- Kali's installer GRUB entries hard-code `console=tty0` — visible in the captured GRUB edit view
  (`serial-grub-entry-kali-arm64.txt`):
  `linux /install.a64/vmlinuz net.ifnames=0 console=tty0 preseed/file=/cdrom/simple-cdd/default.preseed simple-cdd/profiles=kali,offline theme=dark --- quiet`
  so the installer's own output never reaches the serial ptty and cannot be driven headlessly
  without editing that line.

*Recipe for the later task that must verify AC-10:* install Kali once through the UTM window
(spec §1 already allows a manual first install), then with `mode:host` check from inside the guest
that there is no default route, that DNS fails, that `curl http://1.1.1.1` cannot connect and that
`ping` to a LAN peer fails, while `ssh` from the host to the guest's `192.168.x.y` on the vmnet
bridge succeeds — the same battery `docs/verification.md` already ran for the Tart path.
Alternatively, append `console=ttyAMA0,115200` (arm64) / `console=ttyS0,115200` (x86_64) to the
GRUB entry to get a serial-driven installer.

*Rejection path required by FR-6 until that is done:* `isolated` is only offered for
`backend: utm` when the resolved network mode is `host`. If a profile or a host state makes `host`
unavailable, refuse **before creating or starting anything**, name the requested mode, say why it
is unavailable, and state that `--network nat` (UTM `shared`) is an explicit opt-in. Do not fall
back automatically. Until the guest-side check above passes, `doctor --profile` should report UTM
`isolated` as **"mechanism verified, guest isolation unverified"** rather than `ok`, and
`AC-10` stays `unverified`.

### Isolated guests have no DNS, by design — and find that out slowly

Verified from inside a Kali guest on `mode:host` (`task-7-b2b-report.md` §9, AC-10): no default
route, `curl 1.1.1.1` fails, LAN peers answer "Network is unreachable", and host→guest SSH works.
That is the whole point of the mode.

One consequence is worth stating rather than fixing. The `vmnet-host` DHCP lease carries the
**host's own LAN resolvers** (`192.168.50.201`, `192.168.50.1` on the machine this was recorded
on), which are on the other side of the isolation boundary, so the guest's `/etc/resolv.conf` is
full of nameservers it can never reach. Name lookups therefore fail by **timeout** rather than
immediately (`task-7-b2b-report.md` D10).

**wargame leaves that alone on purpose.** An isolated guest is meant to have no name resolution;
rewriting `/etc/resolv.conf` — or pointing it at a resolver on the host — would be a network change
the user did not ask for, made invisibly, in the one mode whose value is that nothing reaches out.
FR-6's rule that a network mode is never silently altered applies to what is inside the guest too.
So: **isolated means no DNS**, and a lookup in an isolated guest is expected to hang for the
resolver timeout and then fail. Use literal addresses in an isolated session, or launch with
`--network nat`, which is the explicit opt-in.

### Guest address discovery

| Mode | How the adapter learns the guest address |
|---|---|
| `host`, `shared`, `bridged` | `utmctl ip-address <id>` / `query ip of vm` — **requires the QEMU guest agent in the guest** (`qemu-guest-agent` package). **Verified** that the channel device is always wired up (`-device virtserialport,chardev=org.qemu.guest_agent,name=org.qemu.guest_agent.0` appears in every generated command line), so only the guest package is missing. |
| `emulated` | No guest IP is reachable from the host at all; use `port forwards` and connect to `127.0.0.1:<host port>`. **Verified** that a forward lands in the argv as `hostfwd=tcp::22022-:22`. |

Because guest-agent installation is part of provisioning (FR-3), the adapter needs a
**port-forward fallback** for the window between "OS installed" and "guest agent running", or it
must drive the first install entirely through the console. FR-4's "allocate a non-conflicting port
per session and store it" applies to `emulated` mode specifically.

## 6. Windows 11 (FR-2)

**No Windows ISO exists on this host and none was downloaded** (the spec says the Windows ISO is
user-provided). `mdfind -name '.iso'` returned no disk image anywhere, and `~/Downloads` is not
readable by this process (`Operation not permitted`). **All Windows guest claims below are
therefore "not verifiable on this host"** and are cited from official documentation.

### What the official docs require

From <https://docs.getutm.app/guides/windows/> ("Windows 11", fetched 2026-09-09):

| Requirement | Official text |
|---|---|
| ISO source | "The easiest way to obtain a Windows installer ISO is with CrystalFetch … Alternatively, you can also download an older ISO directly from Microsoft"; "you must have a valid license from Microsoft to run Windows." |
| Build floor | BSOD `SYSTEM_THREAD_EXCEPTION_NOT_HANDLED` means "You are using a version of Windows that is too old. The build number should be 21390 or higher." |
| Firmware / TPM | "On newer versions of UTM, Secure Boot and TPM should be enabled automatically. You can also enable it manually in Settings under QEMU." Legacy escape hatch: `HKLM\SYSTEM\Setup\LabConfig` with `BypassTPMCheck` / `BypassSecureBootCheck` = 1. |
| TPM availability | UTM 4.3 notes: "TPM 2.0 and Secure Boot … required for Windows 11 updates to 22H2. To enable TPM on an existing virtual machine (only x86_64, i386, and ARM64 architectures are supported), open the VM settings, go to the QEMU page and select 'UEFI Boot', 'TPM 2.0 Device', and 'Reset UEFI Variables.'" |
| Cores | "Windows require at least 2 cores to install… if you are installing x86_64 Windows on an Apple Silicon Mac, the default setting is to use only a single core… you can force multiple cores by going to the System settings and changing the number of cores to 2 or higher and checking 'Force multicore.' Note that you may occasionally experience odd crashes as a result." |
| Drivers / SPICE | wizard step: "Make [sure] 'Install Windows 10 or higher' is checked. Also make sure 'Install drivers and SPICE tools' is checked." Networking: "Make sure you installed the SPICE guest tools, which includes the network drivers." |
| Unattended | UTM 4.0 notes: the guest-tools ISO "include[s] an 'Autounattend.xml' which is recognized by the Windows 10/11 installer. When mounted to a second CD drive, the installer will install the correct drivers, bypass secure boot/TPM requirements, and launch the SPICE tools installer on first login." |
| OOBE without network | `Shift+F10` → `OOBE\BYPASSNRO`, or on newer builds `start ms-cxh:localonly`. |
| 24H2 black screen | "Windows 11 24H2 has a compatibility issue with the graphics drivers that are provided by the UTM guest tools ISO. You can eject the guest ISO from the CD menu and reboot…" |
| Emulated-mode ping | "due to libslirp limitations, `ping` will not work and so Windows may think that there is still no internet connection." |

### What the scripting API actually exposes — **verified against the sdef**

`qemu configuration` has exactly these properties: `name`, `icon`, `notes`, `architecture`,
`machine`, `memory`, `cpu cores`, `hypervisor`, `uefi`, `directory share mode`, `drives`,
`network interfaces`, `serial ports`, `displays`, `qemu additional arguments`.

| Windows need | Scriptable? | Evidence |
|---|---|---|
| UEFI boot | **yes** — `uefi:true` | verified; produces `-drive if=pflash,…,file.filename=…/edk2-x86_64-code.fd` |
| ≥ 2 CPU cores under x86_64 emulation | **yes** — `cpu cores:4` | verified: the x86_64 TCG VM's argv is `-smp cpus=4,sockets=1,cores=4,threads=1`. Note this contradicts the docs' "default … only a single core" / "Force multicore" story — there is no `force multicore` property in the dictionary, and UTM 4.7.5 emitted 4 cpus anyway. The docs' correctness caveat ("odd crashes") still stands; **unverified** whether a Windows guest is stable this way. |
| `hypervisor:false` for x86_64 | **yes** | verified, `-accel tcg,tb-size=1024` |
| Second CD for the guest-tools / `Autounattend.xml` ISO | **yes** | verified (§2, "Attach a *second* ISO") |
| **TPM 2.0** | **NO** | `tpm` is not a property: `update configuration … with (c & {tpm:true})` → `-1700 Can't make {… tpm:true} into type qemu configuration or apple configuration` (fixture `applescript-error-unknown-property-tpm.txt`). `grep -inE 'tpm\|rtc\|local time\|secure boot' utm-4.7.5.sdef` returns only two lines, `UTMScriptingImportCommand` and `UTMScriptingExportCommand`, which match on the `rtC` inside "ImportCommand" — i.e. **zero real hits**. |
| **Secure Boot** | **NO** | same grep; the firmware choice is not scriptable either, although the bundle ships both `edk2-x86_64-code.fd` and `edk2-x86_64-secure-code.fd`, and only the non-secure one was selected for our `uefi:true` VM |
| `rtc local time` | **NO** | not in the dictionary (the field named in the task brief does not exist in UTM's sdef; it belongs to other hypervisors) |
| Downloading the guest-tools ISO | **NO** | that is a UTM UI action ("disk icon in VM window"); nothing in the dictionary triggers it |

**`qemu additional arguments` is not a workaround for TPM.** UTM does bundle
`Contents/Frameworks/swtpm.0.framework`, so the capability exists in the app, but the socket path,
lifecycle and `-tpmdev`/`-device tpm-tis` wiring are managed by UTM's own GUI toggle
(`isTPMDevice`), which the scripting interface does not expose. Injecting raw TPM arguments would
require us to run and supervise `swtpm` ourselves, inside UTM's sandbox, against paths we cannot
read (§1).

### Consequence for the plan

The verified position is: **a Windows 11 VM created purely by script gets UEFI but no TPM 2.0 and
no Secure Boot.** The three viable routes, in preference order:

1. **Mount the UTM guest-tools ISO as a second CD** so `Autounattend.xml` runs. Per the UTM 4.0
   notes this both installs the drivers and "bypass[es] secure boot/TPM requirements". Attaching a
   second removable drive is verified; obtaining the guest-tools ISO is a UI action, so the user (or
   a documented one-time step) supplies its path, exactly like the Windows ISO. **Unverified
   end-to-end.**
2. **`LabConfig` registry bypass** during setup (official, documented above). Manual, in the
   console. **Unverified.**
3. **User enables "TPM 2.0 Device" + "Secure Boot" once in UTM's settings UI** on the installation
   VM, after `profile create` made it. Our `duplicate` of that VM inherits the setting (clones copy
   the whole configuration — verified for every other field). This is the cleanest fit with the
   spec's "first install may be manual through the UTM console", and it should be printed as a
   required step by `profile create` for `os: windows`. **Unverified.**

`doctor --profile` for a Windows profile should state plainly that TPM/Secure Boot cannot be set
programmatically on UTM 4.7.5 and point at the chosen route. This is the one place where the
spec's "Windows 설치 요건을 실제 설치로 검증한다" cannot be satisfied from Task 1 alone.

## 7. Kali Linux — boot verification

### ISO records

Downloaded from the official mirror to `~/Library/Application Support/wargame-vm/iso/` (created by
this task; **not** committed). Both checksums match kali.org's published `SHA256SUMS`
(`shasum -a 256 -c` → `OK` for both).

| File | URL | Size (bytes) | SHA-256 |
|---|---|---|---|
| `kali-linux-2026.2-installer-arm64.iso` | `https://cdimage.kali.org/current/kali-linux-2026.2-installer-arm64.iso` | 3 972 624 384 | `b9a08050ee522fbee7cac703b1bc48178f79eb974c962d4ed9dc1ccfdfa77fb6` |
| `kali-linux-2026.2-installer-amd64.iso` | `https://cdimage.kali.org/current/kali-linux-2026.2-installer-amd64.iso` | 4 802 531 328 | `6dbefacc95e3b556c19c48e8bae39b8b505e2d3a1aba0bfb7ab62b036c3d2ba3` |

Expected digests came from `https://cdimage.kali.org/current/SHA256SUMS` (committed as
`kali-2026.2-installer-sha256sums.txt`). Both files took **342 s** to fetch sequentially over
`curl`. The `.gpg` signature of `SHA256SUMS` was **not** checked — a gap worth closing when FR-2's
"사용자가 제공한 SHA-256은 검증한다" is implemented.

### Kali arm64, `virtualize` (`hypervisor:true`, `-accel hvf`)

`utmctl start` → `started` in **2.3 s**; first firmware byte on the serial ptty at **8.3 s**; GRUB
menu at **9.0 s**.

```
BdsDxe: starting Boot0001 "UEFI QEMU QEMU USB HARDDRIVE 1-0000:00:02.0-4.1" from PciRoot(0x0)/…
GNU GRUB  version 2.14-2+kali1
      Press enter to boot the selected OS, `e' to edit the commands
```

Generated command line (excerpt): `-cpu host -smp cpus=4,… -machine virt -accel hvf`,
`-device usb-storage,drive=…,removable=true,bootindex=0` + `-drive if=none,media=cdrom,…,file.filename=…/kali-linux-2026.2-installer-arm64.iso`.
The removable drive defaults to the **USB** interface on `aarch64`.

### Kali amd64, `emulate` (`hypervisor:false`, `-accel tcg`)

`utmctl start` → `started` in **6.4 s**; first firmware byte at **21.3 s**; GRUB menu at
**31.0 s** — roughly **3.4×** the arm64 milestone. Generated command line (excerpt):
`-cpu Skylake-Client -smp cpus=4,sockets=1,cores=4,threads=1 -machine q35,vmport=off,i8042=off,hpet=off -accel tcg,tb-size=1024`,
firmware `edk2-x86_64-code.fd`, ISO attached as `-device ide-cd,bus=ide.0,…` (removable drives
default to **IDE** on `q35`).

```
BdsDxe: starting Boot0001 "UEFI QEMU DVD-ROM QM00001 " from PciRoot(0x0)/Pci(0x1F,0x2)/Sata(0x0,0xFFFF,0x0)
error: ../../../grub-core/video/video.c:782:no suitable video mode found.
GNU GRUB  version 2.14-2+kali1
```

The `no suitable video mode` lines are expected: this VM had no display device. **AC-02's
"hypervisor off" half is satisfied at the configuration and command-line level**; the guest-side
`/proc/cpuinfo` check still needs an installed OS.

Note that this is as far as the serial line gets on amd64: the menu above is the *firmware's*, and
the ISO's own GRUB then switches to `gfxterm` and stops both reading from and writing to the ptty.
See §3's trap on the stock amd64 ISO — an install here needs a modified ISO or a human at the
window.

### What was deliberately not done

No OS was installed. Getting past GRUB headlessly is blocked by the `console=tty0` kernel argument
baked into Kali's GRUB entries (§5), and the spec assigns the first install to a manual console
session anyway. `input scan code` / `input keystroke` do reach the guest (GRUB redrew in response,
exit 0) but driving a full text install through them was out of scope.

### Boot timeout guidance

FR-5 proposes 1800 s for `emulate` and 300 s for `virtualize`. Measured firmware-to-GRUB is 9 s
and 31 s respectively, so both defaults are generous for the *firmware* stage — but a full Kali
install and first boot under TCG was not measured, so **1800 s stays an unverified estimate**.
Worth re-measuring once a real install exists.

## 8. Reproduction

```bash
# 1. install and identify
brew install --cask utm
utmctl version
sdef /Applications/UTM.app > utm.sdef

# 2. fetch and verify the Kali ISOs
ISODIR="$HOME/Library/Application Support/wargame-vm/iso"; mkdir -p "$ISODIR"
curl -fsSL -o "$ISODIR/kali-linux-2026.2-installer-arm64.iso" \
  https://cdimage.kali.org/current/kali-linux-2026.2-installer-arm64.iso
curl -sSL https://cdimage.kali.org/current/SHA256SUMS \
  | grep -E 'installer-(amd64|arm64)\.iso$' > "$ISODIR/SHA256SUMS.filtered"
(cd "$ISODIR" && shasum -a 256 -c SHA256SUMS.filtered)

# 3. make sure the app is up before any command (trap T8)
open -a UTM; until utmctl version >/dev/null 2>&1; do sleep 1; done

# 4. create, boot, observe, delete
osascript crates/wargame-utm/tests/fixtures/applescript/create-qemu-vm.applescript \
  wargame-t1-kali-arm64 aarch64 true "$ISODIR/kali-linux-2026.2-installer-arm64.iso"
utmctl start wargame-t1-kali-arm64
utmctl status wargame-t1-kali-arm64                       # started
SER=$(osascript crates/wargame-utm/tests/fixtures/applescript/serial-port-address.applescript \
  wargame-t1-kali-arm64)
timeout 25 cat "$SER" | tr -d '\000' | cat -v            # UEFI + GRUB
ps -Aww -o command | grep qemu-aarch64-softmmu | grep -v grep   # full QEMU argv
utmctl stop wargame-t1-kali-arm64 --kill
until [ "$(utmctl status wargame-t1-kali-arm64)" = stopped ]; do sleep 1; done
utmctl delete wargame-t1-kali-arm64
utmctl list                                               # must not list wargame-t1-*
```

`kindle-vm` is a **Tart** VM and never appears in `utmctl list`; nothing in this task touched it.
All `wargame-t1-*` VMs created here were deleted (`utmctl list` empty at the end of the run).

## 9. Open items for later tasks

| # | Item | Owner task |
|---|---|---|
| 1 | Guest-side isolation for `mode:host` (AC-10) — needs an installed Kali | real-VM verification |
| 2 | Windows 11 TPM/Secure Boot route (§6) — pick one of the three and verify end to end | Windows install task |
| 3 | QEMU guest-agent install in Kali/Windows provisioning, so `ip-address`/`exec` work (FR-4) | provisioning task |
| 4 | Port-forward fallback for the pre-guest-agent window, with per-session port allocation (FR-4) | adapter task |
| 5 | `SHA256SUMS.gpg` verification for downloaded ISOs (FR-2) | adapter task |
| 6 | Re-measure the `emulate` boot timeout against a real installed guest (FR-5's 1800 s) | real-VM verification |
| 7 | `--disposable` / `--recovery` / `stop --request` / guest file push-pull / USB — dictionary entries never exercised | as needed |
| 8 | Orphan-QEMU detection after stop (T12) — match `qemu-*-softmmu` argv `-uuid` against the VM id, and a recovery path for the `efi_vars.fd` lock | adapter task |
| 9 | `backend: unavailable` — the third value of the `backend` enum was never reproduced; find its trigger and map it to the session `Missing` state | adapter task |
