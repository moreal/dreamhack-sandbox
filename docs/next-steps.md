# Next steps (handoff for a fresh session)

State on **2026-09-09**: the multi-OS feature is implemented on `feature/multi-os-vm` (HEAD `4b4d825`,
53 commits on top of `main` `d547962`) and **not merged**. The MVP on `main` is untouched and still
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

Verified on real hardware: AC-01, AC-02 (Kali x86_64), AC-03, AC-05, AC-07, AC-10, AC-11, AC-12 — plus
live per-verb proof that `delete` and every other AppleScript verb now touch exactly one VM.

**Not verified, and not to be reported as done:** AC-04's x86_64 half, AC-06, AC-09's corrected-rebuild
half (all three failed on defects that are now fixed but have not been re-run against a guest), AC-08's
TUI half, and **everything Windows, end to end**. `docs/verification.md` marks each one and carries the
placeholder `_(re-run pending — to be filled from task-7-rerun-report.md)_` where a re-run must fill it.

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

# 4. the UTM adapter on its own (opt-in twice: #[ignore] plus two env vars)
WARGAME_E2E=1 WARGAME_E2E_UTM_PROFILE=t7-kali-arm64 \
  nix develop -c cargo test -p wargame-utm --test e2e -- --ignored --nocapture
# read-only first pass, creates nothing:
WARGAME_E2E=1 WARGAME_E2E_UTM_PROFILE=t7-kali-arm64 \
  nix develop -c cargo test -p wargame-utm --test e2e \
    utm_reports_the_recorded_vms_read_only -- --ignored --nocapture
```

Add `WARGAME_BIN=$PWD/result/bin/wargame` to make the scripts drive the Nix-built binary instead of a
`cargo build`. `scripts/e2e-multi-os.sh` fails fast, before creating anything, if the profile's image is
not `ready`, and prints `UNVERIFIED: <why>` for any check it cannot perform — "passed" and "passed
without checking the guest" never read the same.

**The first OS install is not in any of those.** It is a manual step —
`wargame profile create <p> --iso <path>` → `wargame profile console <p>` (install) →
`wargame profile eject <p>` → `wargame profile build <p>` — and the method that worked here (unattended
preseed over the UTM serial ptty, with every trap it hit) is recorded in `docs/verification.md`, "The
ISO installs, as a separate manual step", and in full in
`.superpowers/sdd/PLAN/task-7-phase-a-report.md`.

## The dedicated verification home

Every multi-OS command used `WARGAME_HOME="$HOME/Library/Application Support/wargame-vm-t7"` so the
user's real home was never written. It holds:

- profiles `t7-kali-arm64` (utm/virtualize), `t7-kali-x86_64` (utm/emulate) and `t7b-kali-arm64`, plus
  their per-profile provisioning script copies and the four built-in templates. **`resources.disk_gib`
  is 20 in these copies, not the templates' 80** — a deliberate edit for this host's free space; the
  repository templates are untouched.
- `keys/id_ed25519(.pub)` — the key the installed guests trust in `~kali/.ssh/authorized_keys`. A fresh
  `WARGAME_HOME` would have a different key and could not log into those guests.
- `builds/<profile>/record.json` and `logs/provision-<profile>.log`.
- Session records left behind by the acceptance run, all pointing at VMs that no longer exist. Removing
  the `sessions/` directory by hand is safe.

The build records went **stale** when bug 20 destroyed every UTM VM; a reinstall of both Kali guests was
under way when this was written (the ledger records arm64 finished, x86_64 in progress). Confirm with
`utmctl list` and `wargame profiles` before trusting any `ready`/`missing` word, and expect to run
`wargame profile create` again for any profile whose installation VM is gone.

## Open items, in suggested order

1. **Finish the re-run on the reinstalled guests.** In order: `profile build` (an IPv4 address is waited
   for, the first `scp` lands, the image publishes as `wg-build-image-<profile>-<hex>`); a **second**
   `profile build` of the same profile (it publishes, the record swaps images, the previous image is
   deleted only afterwards, `leaked_images` ends empty) — that closes AC-06 and AC-09; `launch` → `ssh`
   → workspace → `stop` → `resume` → **`gui` with `uptime` unchanged** → `ssh`; two sessions from one
   image on the isolated network (different MACs, different IPs, each `scp` landing in the right guest);
   then `delete`/`prune` with the installation VM and the image present throughout. The x86_64 `launch`
   is the AC-04 row that is still open. Write the results into `docs/verification.md` in place of the
   `_(re-run pending …)_` placeholders.
2. **Windows, end to end — the largest gap.** It needs a user-supplied Windows 11 ISO (the spec makes it
   user-provided; nothing was downloaded or simulated). Then: `profile create windows-x86_64 --iso …`,
   **do the printed manual step** — UTM 4.7.5's scripting dictionary has no TPM and no Secure Boot, so
   `profile create` prints "open the VM in UTM > Settings > QEMU and enable UEFI Boot, TPM 2.0 Device
   and Reset UEFI Variables" as a required step before installing — install, `profile eject`,
   `profile build`, then a launch. This is the first execution of `guest/bootstrap.ps1` and the
   PowerShell transport anywhere, and the first exercise of `windows-x86_64.yaml`'s `tools.verify`
   (which currently ships empty). Expect to find things; budget for it.
3. **Guest-side isolation is now verified** — no action, but worth knowing when reading older text:
   `doctor`'s `network:isolated:utm` row and earlier documents said "mechanism verified, guest isolation
   unverified". The acceptance run proved it from inside the guest (no default route, no DNS, no
   internet, the host's own LAN unreachable, host↔guest SSH working). Any remaining "unverified"
   wording about UTM isolation is stale.
4. **Deferred minors that a user can notice**, from the ledger (`.superpowers/sdd/PLAN/progress.md`):
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
     guidance offers `profile eject` for a `--from-vm` profile whose media state is `Unknown`.
5. **Before merging:** re-read spec §9's checklist against `docs/verification.md`, enable
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
