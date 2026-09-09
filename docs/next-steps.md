# Next steps (handoff for a fresh session)

State on **2026-09-09**: the multi-OS feature is implemented on `feature/multi-os-vm` (HEAD `1998a92`,
55 commits on top of `main` `d547962`) and **not merged**. The MVP on `main` is untouched and still
works exactly as `docs/verification.md`'s first half records; the five open items that used to live in
this file were all closed on 2026-09-06 and their evidence is in that record.

Read, in order: `docs/verification.md` (both records — what is verified, what is *not*, and by what),
`.superpowers/sdd/PLAN/spec.md` (§7 acceptance table, §9 completion checklist), `DESIGN.md` (§3.7–§3.9,
§8 and §10 are the multi-OS architecture and verification posture), `docs/utm-compatibility.md` (the UTM
automation contract, with committed fixtures for every claim), `README.md` (user guide and the four
real-hardware test layers).

## Where the branch stands

Implemented and green on the automated gate: a second backend (`wargame-utm`, `utmctl` + AppleScript),
profile-driven backend/mode selection with no silent fallback, ISO-based `profile create` and
`--from-vm` adoption, `profile console|eject|stop|delete`, app-owned build records with image
preservation, per-OS guest transports (POSIX and PowerShell), Kali and Windows profile templates, and
the multi-OS lifecycle across mixed backends.

**Every acceptance criterion except the Windows rows now passes on real hardware.** AC-01 through AC-12
were verified across three real-VM passes — the acceptance run, a second pair of Kali installs, and a
targeted re-run that re-drove everything the acceptance run had left open (AC-04 on both architectures,
AC-06, AC-08, AC-09) and re-confirmed nine of that run's ten defects fixed on live VMs, including the
delete cascade and the installer-disc eject on installed guests.

**Not verified, and not to be reported as done:** **everything Windows, end to end** (no ISO on this
host — it is user-provided by spec, and nothing was simulated); AC-06 on x86_64 (the custom-script
rebuild ran on arm64 only); and AC-08's TUI half, which refuses to render non-interactively.
`docs/verification.md` has the per-row detail and an "Explicitly unverified" list; three items the
re-run opened (N1, N2, N3) are recorded there too.

## How to re-verify

```bash
# 1. the automated gate — no VM, no UTM, no account (spec §9)
nix develop -c cargo fmt --all --check
nix develop -c cargo clippy --workspace --all-targets -- -D warnings
nix develop -c cargo test --workspace
nix flake check
nix build                                   # use result/bin/wargame for every real run

# 2. the macOS/Tart regression (needs the built macOS profile image in the default home)
nix develop -c scripts/e2e-smoke.sh macos-minimal-arm64

# 3. any profile whose image is already built — the multi-OS one
export WARGAME_HOME="$HOME/Library/Application Support/wargame-vm-t7"
nix develop -c scripts/e2e-multi-os.sh t7-kali-arm64
nix develop -c scripts/e2e-multi-os.sh t7-kali-x86_64
WARGAME_E2E_NETWORK=isolated nix develop -c scripts/e2e-multi-os.sh t7-kali-arm64

# 4. the UTM adapter on its own (opt-in twice: #[ignore] plus two env vars).
#    --test-threads=1 is REQUIRED: concurrent osascript against one UTM collapses with -609,
#    and one test's "the VM count did not change" invariant is global (open defect N1).
WARGAME_E2E=1 WARGAME_E2E_UTM_PROFILE=t7-kali-arm64 \
  nix develop -c cargo test -p wargame-utm --test e2e -- --ignored --test-threads=1 --nocapture
# read-only first pass, creates nothing (currently fails on defect N2, not on the product):
WARGAME_E2E=1 WARGAME_E2E_UTM_PROFILE=t7-kali-arm64 \
  nix develop -c cargo test -p wargame-utm --test e2e \
    utm_reports_the_recorded_vms_read_only -- --ignored --nocapture
```

Add `WARGAME_BIN=$PWD/result/bin/wargame` to make the scripts drive the Nix-built binary instead of a
`cargo build`. `scripts/e2e-multi-os.sh` fails fast, before creating anything, if the profile's image is
not `ready`, and prints `UNVERIFIED: <why>` for any check it cannot perform — "passed" and "passed
without checking the guest" never read the same.

**The first OS install is not in any of those.** It is a manual step —
`wargame profile create <p> --iso <path>` → `wargame profile console <p>` (install the OS, then shut the
guest down from inside the installer) → `wargame profile eject <p>` **before it boots again** →
`wargame profile build <p>`. That order matters: both real guests rebooted into the attached installer,
and the x86_64 one started re-running an unattended install over the disk it had just written. Since
`1998a92` the `profile create` checklist says so. The method that worked here (unattended preseed over
the UTM serial ptty, with every trap it hit) is recorded in `docs/verification.md`, "The ISO installs, as
a separate manual step", and in full in `.superpowers/sdd/PLAN/task-7-phase-a-report.md` and
`task-7-phase-a2-report.md` — the second install repeats the recipe on the fixed CLI and takes
**771 s (arm64) / 1 960 s (x86_64)** when run sequentially on an idle host.

## The dedicated verification home

Every multi-OS command used `WARGAME_HOME="$HOME/Library/Application Support/wargame-vm-t7"` so the
user's real home was never written. It holds:

- profiles `t7-kali-arm64` (utm/virtualize) and `t7-kali-x86_64` (utm/emulate) — re-created from the
  templates for the second install, so they carry the shipped `disk_gib: 80` (each finished guest
  occupies under 2 GiB of it). **`t7-kali-arm64.yaml` deliberately keeps the AC-06 additions**
  (`profiles/scripts/custom/my-tools.sh` plus the `ltrace`/`strace` verify lines): its current image was
  built from them, so reverting either makes the profile read `rebuild needed`.
- `keys/id_ed25519(.pub)` — the key the installed guests trust in `~kali/.ssh/authorized_keys`. A fresh
  `WARGAME_HOME` would have a different key and could not log into those guests.
- `builds/<profile>/record.json` (both `phase: ready`, `leaked_images: []`) and
  `logs/provision-<profile>.log`.
- No session records: the re-run deleted every session it made.

**Current end state**, and what a next session inherits: UTM holds exactly four stopped VMs — the two
installation VMs (`wg-build-t7-kali-arm64` `9B71AC04-…`, `wg-build-t7-kali-x86_64` `4EE44988-…`) and the
two published images (`…-arm64-62cc30` `890D47C7-…`, `…-x86_64-70bde9` `319AACC2-…`) — no orphan QEMU
process, Tart untouched, 98 GiB free on `/` (the low-water mark during the re-run was 14 GiB). Guest
login is `kali` with passwordless sudo and that key; guest IPs are DHCP and change between boots, so
always re-read them with `utmctl ip-address <uuid>` (it prints three lines: IPv4, global IPv6,
link-local).

## Open items, in suggested order

1. **Two open defects in the opt-in test surface** (neither is in the product; both found by the
   re-run). **N1**: the gated UTM e2e suite cannot run in parallel — concurrent `osascript` against one
   UTM collapses with `-609`, and `utm_never_deletes_a_vm_it_does_not_own`'s "the VM count did not
   change" invariant is global, so another test's clone breaks it; the invocation in the file's own
   module comment carries no `--test-threads=1`, so the documented command fails on a healthy host.
   **N2**: `utm_reports_the_recorded_vms_read_only` can never pass since the D7 fix — the fixture builds
   the provider without `.with_image_catalog(…)`, so `image_exists` asks a `NoBuildRecords` store and
   always answers `false`; the fixture already loads the real `JsonBuildRepository` and just never hands
   it over. Also worth watching: **N3**, one non-reproducible `pausing` → `stopped` right after the
   first `gui` (five later calls did not reproduce it, and the code path cannot stop a VM — most likely
   UTM-side; cause unverified).
2. **Windows, end to end — the largest gap.** It needs a user-supplied Windows 11 ISO (the spec makes it
   user-provided; nothing was downloaded or simulated). Then: `profile create windows-x86_64 --iso …`,
   **do the printed manual step** — UTM 4.7.5's scripting dictionary has no TPM and no Secure Boot, so
   `profile create` prints "open the VM in UTM > Settings > QEMU and enable UEFI Boot, TPM 2.0 Device
   and Reset UEFI Variables" as a required step before installing — install, `profile eject`,
   `profile build`, then a launch. This is the first execution of `guest/bootstrap.ps1` and the
   PowerShell transport anywhere, and the first exercise of `windows-x86_64.yaml`'s `tools.verify`
   (which currently ships empty). Expect to find things; budget for it.
3. **Close AC-06 on x86_64** if you want the row complete: add a custom provisioning script and two
   `tools.verify` lines to `t7-kali-x86_64` and rebuild. It is the same path already proven on arm64
   (four rebuilds, no name collisions) and costs about six minutes under TCG.
4. **Guest-side isolation is now verified** — no action, but worth knowing when reading older text:
   `doctor`'s `network:isolated:utm` row and earlier documents said "mechanism verified, guest isolation
   unverified". The acceptance run proved it from inside the guest (no default route, no DNS, no
   internet, the host's own LAN unreachable, host↔guest SSH working). Any remaining "unverified"
   wording about UTM isolation is stale.
5. **Deferred minors that a user can notice**, from the ledger (`.superpowers/sdd/PLAN/progress.md`):
   - **Tart has no "previous image preserved on failure".** `TartImageBuilder::prepare` deletes the
     image before cloning, so a failed Tart rebuild leaves the profile with no image (it fails loudly:
     `launch` → `ImageMissing`, `profiles`/`doctor` show it not built). UTM does preserve it. The fix is
     publish-to-temp-then-rename.
   - **The disk-space check still measures `$WARGAME_HOME`'s volume for Tart** — correct for UTM since
     `a6bcd2b`/`14e799c` (it measures UTM's container volume), but Tart answers no `storage_dir`, and
     Tart's VMs live under `~/.tart`. Wrong number on a host that splits those volumes.
   - **`image_state` can be `null` in `profiles --json`** when the backend lookup fails. Since
     `4b4d825` that case reads `unknown` in the table with the backend's own message and a new
     `image_error` field in JSON, but a consumer that assumes a string still needs to handle `null`.
   - **`WARGAME_VM=fake` has no fake image builder**, so `profile build` cannot be rehearsed without a
     real backend.
   - **`app/profile_build.rs` is ~1300 lines**; the record state machine and the build flow were meant
     to split into `app/build_flow.rs`.
   - Smaller: `prune` can never delete a VM whose session record is gone (it is reported as unmanaged
     forever — the price of record-based ownership); `doctor` exits 0 even when checks fail;
     `wargame inspect` is not a top-level alias for `wargame session inspect`; `profile console`'s
     guidance offers `profile eject` for a `--from-vm` profile whose media state is `Unknown`; a guest
     disk's `host size` reads 0 while its VM runs, so read it stopped.
6. **Before merging:** re-read spec §9's checklist against `docs/verification.md`, enable
   `sacho.toml`'s `[check] paths` (left empty on purpose — it would fail `sacho check --base main`
   against this branch's existing commits), and decide whether an unverified Windows path blocks the
   merge or ships documented.

## Host facts that matter

- **UTM 4.7.5**, installed with `brew install --cask utm`; `utmctl` lives inside `UTM.app` and is
  cask-symlinked to `/opt/homebrew/bin/utmctl`. It needs **no sudo and no sudoers rule** — UTM carries
  the virtualization and networking entitlements itself. Automation permission is granted per
  *responsible app*, so running `wargame` from a different terminal will prompt once; a denial surfaces
  as `-1743` and now reads as an `unknown` image state rather than a missing image.
- **UTM has no headless start.** Any UTM operation launches the app and any start opens a window. That
  is documented behaviour, not a defect — but these runs are not unattended-friendly on a shared
  desktop.
- `/etc/sudoers.d/wargame-softnet` grants passwordless sudo for `/opt/homebrew/bin/softnet` only. That
  is the **Tart** isolated path; UTM's isolated mode (`host` = `vmnet-host`) does not use Softnet.
- **The changelog is `sacho`'s.** sacho 0.3.2 is pinned project-locally in the repo's `mise.toml`
  (the user's global mise config is untouched). One fragment per user-visible change under `changes.d/`,
  committed with the code: `sacho add <topic>`, `sacho fmt`, `sacho check`. `CHANGELOG.md` is generated
  (`sacho sync --force`) and is **never hand-edited**. A fresh clone needs `sacho init` (it writes merge
  drivers into the repo-local `.git/config`, which is not committed).
- **`kindle-vm` is the user's own Tart VM and must never be stopped, deleted or referenced.** It has no
  `wg-` prefix, so the adapters' namespace floors ignore it, and every run so far confirmed it untouched.
- The user's real home `~/Library/Application Support/wargame-vm` holds the two built macOS profile
  images, the guest SSH key those images trust, and the user's own **stopped session
  `dh-sprout-base64-3edf2c`** — never delete it. The Dreamhack session cookie is in the keychain; the
  user prefers to run `wargame auth login` themselves in a real terminal (the password prompt needs a
  TTY).
- Use the `nix build` binary (`result/bin/wargame`) for real runs and rebuild it after code changes;
  `profile build` embeds its provisioning scripts, so no checkout path is needed at runtime.
- Disk is the binding constraint on this host (~16–25 GiB free through the acceptance run). UTM guest
  disks are sparse qcow2 and grow as they are used; `profile create` refuses only below an 8 GiB floor
  and explains the rest.
