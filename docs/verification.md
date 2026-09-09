# Verification record — 2026-09-05 / 2026-09-06

Host: macOS 26.6.2, Apple Silicon, Tart 2.36.0 (via nixpkgs wrapper) / 2.32.1 (Homebrew), Determinate Nix 3.21.9.
Branch: `feat/mvp`, merged fast-forward into `main` on 2026-09-06.

## Build and static checks

| Check | Command | Result |
|---|---|---|
| Format | `nix develop -c cargo fmt --all --check` | clean |
| Lints | `nix develop -c cargo clippy --workspace --all-targets -- -D warnings` | clean |
| Unit + fixture + CLI tests | `nix develop -c cargo test --workspace` | all green (≈230 tests across 4 crates), 1 ignored (the real-VM e2e), 0 failed |
| Reproducible build | `nix build` | `result/bin/wargame` (wrapped with `tart`, `ssh`, `sshpass` on PATH) |
| Flake checks | `nix flake check` | fmt, clippy, build all ✅ (aarch64-darwin) |

## Functional checks without a VM (`WARGAME_PROVIDER=fixture WARGAME_VM=fake`)

- `wargame search hello` → lists `fixture:1 hello-fixture`.
- `wargame open fixture:1` → description and asset list.
- `wargame launch fixture:1 --profile macos-minimal-arm64 --yes` → events in order (session created, VM created, downloaded with SHA-256, booting, guest ip, bootstrapping, Ready).
- `wargame sessions`, `session inspect --json`, `stop`, `resume`, `delete` → state transitions ready → stopped → running → deleted.
- `wargame launch … --network nat` without `--yes` and without a TTY → prints the NAT warning and exits 1 (MVP §18 confirmation).
- TUI driven through a sized pty: search → problem screen (Environment / Network pickers) → launch reaching Ready → sessions screen → quit, no panic.

## Real host checks

- `wargame doctor` on the real host: platform, tart, ssh/scp/ssh-keygen, security, sshpass, disk space, Dreamhack reachability all OK; profile images and guest key reported missing with the `wargame profile build` hint.
- **M0 VM proof** (`WARGAME_E2E=1 cargo test -p wargame-tart --test e2e -- --ignored`): clones the cached `ghcr.io/cirruslabs/macos-sequoia-vanilla:latest`, boots headless, waits for SSH, copies a fixture zip, runs `guest/bootstrap.sh`, verifies the extracted file, stops and deletes the VM. Passed in 31.6 s; `tart list` shows no leftover `wg-` VM.

### Bug found and fixed by the real-VM run

`tart set --disk-size` refuses a size that is not strictly larger than the clone's current disk. The first e2e run failed with `new disk size of 40 GB should be larger than the current disk size of 50 GB`; the delete-on-failure path cleaned up correctly. Fix (commit `822be92`): `TartProvider::create` now reads the clone's current disk from `tart list --format json` and passes `--disk-size` only when the profile would grow it.

## Profile build and full smoke (real Tart)

`wargame profile build macos-minimal-arm64` against the cached vanilla image, then the fixture challenge launched into a clone of that image over NAT.

| Step | Result |
|---|---|
| `profile build macos-minimal-arm64` | ✓ built in 68 s (Homebrew + git, python 3.14, uv, curl, wget, jq, vim, tree); log at `$WARGAME_HOME/logs/provision-macos-minimal-arm64.log` |
| `scripts/e2e-smoke.sh macos-minimal-arm64` | ✓ passed in 20 s: launch → clone → download (SHA-256 recorded) → boot → SSH → copy-in → guest bootstrap → Ready → file verified over SSH → delete |
| Fresh clone tool check | `uv 0.12.10`, `Python 3.14.7`, `git 2.55.0`, `jq 1.8.2` on PATH via `ssh host cmd`; provisioning directory removed |
| `stop` / `resume` / `prune --dry-run` / `delete` on a live session | ✓ (resume in 11 s; extracted file persisted across the restart; no orphan VMs) |
| `launch` with default `isolated` network on this host | refused in preflight (no session or VM created) with the Softnet sudoers hint, because passwordless sudo is not configured here |

### Bugs found and fixed by the real runs (all in `feat/mvp`)

1. `tart set --disk-size` rejects a size not larger than the clone's current disk → pass it only when growing (`822be92`).
2. Provisioning paths used a literal `~` inside a quoted SSH argument, `scp -r` into a missing directory renamed the tree, and a failed build left the image behind → home-relative paths, `mkdir -p` first, stop+delete on failure (`fce62f2`).
3. `ssh host cmd` runs a non-login zsh that never reads `~/.zprofile`, so Homebrew tools were not on PATH; guest script output was not captured → also write `~/.zshenv`, log every guest command to `provision-<profile>.log` (`1977720`).
4. Tart's `--net-host` runs Softnet, which needs root; without it the runner died instantly and launch waited the full 300 s → `doctor` check `network:isolated`, launch preflight refusal with remediation, and fail-fast when the runner exits during boot (`c61cb32`).
5. `tart stop` force-kills after 30 s, losing the tail of the provisioning writes (python/uv missing from the image) → `sync` in the guest, `--timeout 180`, poll until stopped (`7e2a994`).

## Real Dreamhack run — 2026-09-06

`wargame auth login` (interactive, in a terminal), then `wargame launch dreamhack:15 --profile macos-minimal-arm64 --network nat --yes` (rev-basic-1, Bronze 4 reversing).

| Step | Result |
|---|---|
| Download shape recon (no credentials) | Frontend API wrapper (`static.dreamhack.io/main/v2/js/2e5a94f.js`) does a bodiless `axios.post` and returns `response.data`; the challenge-page chunk (`ebda421.js`, found via the webpack chunk map in `f0efc1a.js`) assigns that value straight to `<a href>` and clicks it → the body is a bare JSON string URL |
| First launch | ✗ `authentication required` at the download step although `auth status` showed a session: a curl replay returned `403 {"detail":"CSRF Failed: CSRF token missing."}` |
| Second launch | ✓ Ready in 23 s: download 5255 B, SHA-256 `15c4ed7a…fc0eb`, boot, SSH, bootstrap; guest has `~/wargame/rev-basic-1/incoming/extracted/…/chall1.exe` (PE32+), but named `4482bbb1-…-d23253b84453.zip` |
| Third launch | ✓ Ready in 23 s, asset staged and extracted as `rev-basic-1.zip` / `extracted/rev-basic-1/chall1.exe`; `stop`, `delete` clean, no `wg-` VM left |

### Bugs found and fixed by the real Dreamhack run (`dfc42c5`)

6. The adapter treated the download response's top-level JSON string as `ProviderChanged`; only object shapes were accepted → `file_refs_from_json` accepts the bare string first, object/`files[]` shapes stay as fallbacks.
7. Dreamhack's CSRF cookie is `csrf_token`, not Django's default `csrftoken`, so no `X-CSRFToken` was sent and the 403 was reported as "authentication required" → `client::csrf_token` tries `csrf_token` then `csrftoken` (login and download); a 403 whose body says `CSRF Failed` is now reported as `ProviderChanged` with the cookie names tried, so it cannot be mistaken for a missing login.
8. Object storage names the file `<uuid>.zip` via `Content-Disposition` → `choose_filename` prefers the asset placeholder (`<repository>.zip`) over a UUID-stem name when the extensions match, so the guest sees `rev-basic-1.zip`.

## Reversing image — 2026-09-06

`wargame profile build macos-re-arm64` with the `nix build` binary, then `wargame launch dreamhack:15 --profile macos-re-arm64 --network nat --yes`. Six image builds in total (≈2.5–3 min each against the cached vanilla image; Homebrew bottles for Ghidra 12.1.3 + openjdk@21, LLVM 23.1.0, lld, binutils, hexyl, file).

| Step | Result |
|---|---|
| Build 1 | ✗ before provisioning: `scp: stat local "/nix/var/nix/builds/…/source/crates/wargame-cli/../../profiles/scripts": No such file` (bug 9) |
| Build 2 | ✓ built in 2:42; `ghidraRun` on PATH, but `llvm-objdump`/`lld`/LLVM `clang` missing from PATH (keg-only), Temurin JDK redundant next to openjdk@21 |
| Launch into build 2 | ✓ Ready in 23 s; `analyzeHeadless … -import chall1.exe` → `Analysis succeeded`, `Import succeeded` on the PE32+ challenge binary |
| Build 3 | ✓ built, but the extended `tools.verify` list was silently ignored (bug 10) |
| Build 4 | ✗ `analyzeHeadless`: `Unable to locate a Java Runtime` without Temurin (fixed: JAVA_HOME from openjdk@21) |
| Build 5 | ✗ verify `analyzeHeadless 2>&1 \| grep …`: `Bad argument: grep`, verify entries are argv, not shell; then `which lld` failed: `lld` is its own formula |
| Build 6 | ✓ built in 2:41, all 10 verify commands pass |
| Launch into build 6 | ✓ Ready in 19.5 s. Over non-login `ssh host cmd`: `ghidraRun`, `pyghidraRun`, `analyzeHeadless`, `java` (openjdk 21.0.12.1), `clang` (Homebrew 23.1.0), `lld`, `llvm-objdump`, `llvm-readobj`, `gobjdump`, `hexyl`, `python3`, `uv`, `git` all resolve; Apple `objdump`/`strings`/`file` stay the defaults. `analyzeHeadless gp p -import chall1.exe -deleteProject` → `Analysis succeeded` / `Import succeeded`; `llvm-objdump -d chall1.exe` disassembles 1291 instructions. `delete` clean, no `wg-` VM left |

### Bugs found and fixed by the reversing image build

9. `profile build` located provisioning scripts via a compile-time `CARGO_MANIFEST_DIR` fallback, a sandbox path in a `nix build` binary → scripts are embedded and written to `$WARGAME_HOME/profiles/scripts/` on every run (`083dc66`).
10. Built-in YAMLs seeded into `$WARGAME_HOME/profiles/` were user overrides forever, so a newer built-in never took effect → a pristine copy under `profiles/.builtin/` lets `build_context` refresh unedited YAMLs and leave edited ones alone (`829de02`). On a host seeded by an older binary, delete the unedited YAML once (or the `.builtin/` bookkeeping starts from the next run).
11. Keg-only LLVM/binutils/openjdk and the separate `lld` formula were not on PATH; `reversing.sh` exports `JAVA_HOME` and the PATH additions into `~/.zshenv` + `~/.zprofile`, Temurin dropped (`829de02`).
12. `tools.verify` entries are whitespace-split argv with no shell; the YAML documents it and uses `which`/`--version` checks only (`829de02`).

## Isolated networking — 2026-09-06

The user installed the sudoers rule printed by `wargame doctor` (`moreal ALL=(root) NOPASSWD: /opt/homebrew/bin/softnet`), then `wargame launch dreamhack:15 --profile macos-minimal-arm64 --yes` with the default `isolated` network.

| Step | Result |
|---|---|
| `doctor` | `network:isolated` flipped from the sudoers hint to `ok` with no code change |
| Launch | ✓ Ready in 20.0 s; guest IP `192.168.18.2` on the host-only bridge (NAT launches get `192.168.64.x`). Host shows `tart run … --net-host` and Softnet escalated via `sudo --non-interactive … /opt/homebrew/bin/softnet --vm-net-type host` and running as the user afterwards |
| SSH + staging | ✓ `ssh admin@192.168.18.2` with the guest key; `chall1.exe` extracted under `~/wargame/rev-basic-1/incoming/extracted/rev-basic-1/` (download happened on the host, as designed) |
| Guest isolation | ✓ no default route (`netstat -rn` has none); `dig dreamhack.io` fails; `curl https://dreamhack.io` → could not resolve; `curl http://1.1.1.1` → could not connect; `ping 8.8.8.8` 100 % loss; only the bridge gateway `192.168.18.1` answers |
| `stop` / `resume` | ✓ stop 0.7 s, resume 9.4 s, same guest IP; SSH works and the guest is still cut off after the resume |
| `delete --force` | ✓ no `wg-` VM, no `tart run`/`softnet` process left |

No bugs found; the preflight/doctor/fail-fast work from `c61cb32` behaved as designed once the rule existed.

## `wargame gui` — 2026-09-06

`wargame launch dreamhack:15 --profile macos-re-arm64 --yes` (default `isolated` network) with the `nix build` binary, then `wargame gui <id>` and the lifecycle commands around it. The window itself was observed through Launch Services (`lsappinfo` lists `com.github.cirruslabs.tart`, pid of the new runner, type Foreground, "in front"), not by eye; the user was invited to watch it.

| Step | Result |
|---|---|
| Launch | ✓ Ready in 36 s, guest IP `192.168.18.3`, headless runner `tart run … --no-graphics --net-host` |
| `gui` on the Ready session | ✓ returned in 20 s: old runner gone, new `tart run … --net-host` without `--no-graphics`, Softnet re-escalated, Tart registered as a foreground app; record `ready → running`, same guest IP, `runner_pid` updated to the new runner; `tart list` shows the VM running |
| SSH with the window open | ✓ guest key login, challenge file still under `~/wargame/rev-basic-1/incoming/extracted/rev-basic-1/`, no IPv4 default route, `curl https://dreamhack.io` cannot resolve (still isolated) |
| Closing the window (quit the Tart app) | VM stopped cleanly in Tart (no `tart run`/`softnet` process, `tart list` says stopped) but the record stayed `running`; `stop` failed with `tart stop … failed: VM "…" is not running`, `resume` refused with `Running -> Running` (bug 13) |
| After the fix | `sessions` reported `stopped` without any command touching the record; `resume` ✓ 12 s; `gui` ✓ 20 s |
| `stop` with the window open | ✓ 4.4 s: window and runner gone, VM stopped, record `stopped` |
| `gui` on the Stopped session | ✓ 14 s: window back, record `running`, SSH answers |
| `delete --force` with the window open | ✓ 1.8 s: no `wg-` VM, no `tart run`/`softnet` process, no Tart app, `prune --dry-run` finds nothing |

### Bug found and fixed by the `gui` run

13. The VM can stop without going through the CLI (the Tart window is closed, the guest shuts down, the runner dies) and nothing reconciled the session record, so it stayed `running` and both `stop` (`tart stop` on a stopped VM) and `resume` (`Running -> Running`) failed; only `delete` worked → `lifecycle::reconcile` moves an up record to `Stopped` (and clears `runner_pid`) when the backend reports the VM stopped, and `sessions`/`inspect`/`stop`/`resume`/`ssh`/`gui` all go through it. `stop` on such a session is a no-op that returns the corrected record, and `gui` now accepts a `Stopped` session so the closed window can simply be reopened. A `Missing` VM is left alone (delete/prune territory). Six unit tests cover the drift cases.

## Not verified live

- Ghidra's GUI (`ghidraRun`) inside the guest: only headless analysis was exercised over SSH.
- `scripts/e2e-smoke.sh` with `WARGAME_SMOKE_NETWORK=isolated` (the manual run above covers the same path).

---

# Multi-OS guests (2026-09-09 → …)

Branch `feature/multi-os-vm` (`5f47ce1..4b4d825`, 53 commits), **not merged**. This section records the
UTM/QEMU work — Kali arm64 `virtualize`, Kali x86_64 `emulate`, and the Windows path this host could not
verify. Everything above it is the MVP/Tart record and is unchanged.

Read it with the per-task reports under `.superpowers/sdd/PLAN/` (they hold the raw transcripts this
summarises) and `docs/utm-compatibility.md` (the UTM automation contract, with committed fixtures).

## Host, tools and media

| Item | Value |
|---|---|
| Host | macOS 26.6.2 (25G83), Apple Silicon, 10 cores, 64 GiB RAM |
| UTM | **4.7.5** (`CFBundleVersion` 118), installed with `brew install --cask utm`; `utmctl` inside the bundle, cask-symlinked to `/opt/homebrew/bin/utmctl` |
| Tart | 2.36.0 — what the `nix build` binary wraps and what `doctor` reports (`task-7-ac01-report.md`); 2.32.1 from Homebrew on a plain shell's PATH (`task-7-b2b-report.md`). No UTM work touched Tart |
| Automation permission | no prompt on this host: `utmctl` ships inside `UTM.app` (same-app Apple Events) and the responsible terminal already held Automation → UTM. The grant is per responsible app, so another terminal prompts once. No sudo, no sudoers rule, no Full Disk Access (`task-1-report.md`) |
| Verification home | `WARGAME_HOME="$HOME/Library/Application Support/wargame-vm-t7"` on **every** multi-OS command. The user's default home was read (AC-01) and never written by the UTM work; phase B2c used a throwaway home under the session scratchpad |

### Installer media

Kept at `~/Library/Application Support/wargame-vm/iso/`, never committed:

| File | Size (bytes) | SHA-256 |
|---|---|---|
| `kali-linux-2026.2-installer-arm64.iso` | 3 972 624 384 | `b9a08050ee522fbee7cac703b1bc48178f79eb974c962d4ed9dc1ccfdfa77fb6` |
| `kali-linux-2026.2-installer-amd64.iso` | 4 802 531 328 | `6dbefacc95e3b556c19c48e8bae39b8b505e2d3a1aba0bfb7ab62b036c3d2ba3` |

Both digests match `https://cdimage.kali.org/current/SHA256SUMS` (committed as the fixture
`kali-2026.2-installer-sha256sums.txt`) and were re-verified unchanged at the start and the end of
phase A and of the acceptance run. Both ISOs took 342 s to fetch. The `SHA256SUMS.gpg` signature was
**not** checked — an open gap against FR-2's "사용자가 제공한 SHA-256은 검증한다".

**Windows: no ISO exists on this host.** `mdfind -name '.iso'` found none and `~/Downloads` is not
readable by the agent process; the spec makes the Windows ISO user-provided. Nothing was downloaded and
**nothing about Windows was simulated**.

## Acceptance criteria — verdicts at `4b4d825`

The acceptance run (`task-7-b2b-report.md`) drove the binary built from **`b06c46d`**. The defects it
found were fixed afterwards in `7642e36`, `6db2e92`, `14e799c` and `4b4d825`, and **those fixes have not
been re-run against a real guest** — the run's own critical defect destroyed both installed Kali guests
(bug 20 below) and the reinstall takes ~1 h 20 m (arm64) / ~10 h (x86_64). Every row that a re-run must
settle says so, and is **not** counted as verified.

| ID | Verdict | Evidence |
|---|---|---|
| **AC-01** existing macOS profiles + old sessions | **PASS** (real macOS VMs) | `task-7-ac01-report.md` §2–§4, binary pinned to `eadc3ba`. Both pre-branch macOS profiles launch on the default `isolated` network (Ready 19.9 s / 16.5 s), ssh → stop → resume → delete clean; both YAMLs stay byte-identical; the pre-branch session record loads as tart/arm64/virtualize/admin and its `metadata.json` hash is unchanged; `doctor --profile` shows no UTM row for either. |
| **AC-02** Windows/Kali x86_64 with hypervisor off | **PASS for Kali x86_64 / UNVERIFIED for Windows** | `task-7-b2b-report.md` §3.2 (`logs/18`, `logs/13`): image built and published from the emulated guest in **257 s**; `hypervisor=false` in the AppleScript configuration readback, `-accel tcg,tb-size=2048 -cpu Skylake-Client -machine q35` in the live QEMU argv, `uname -m` = `x86_64` in the guest. **Windows: unverified** — no ISO on this host, user-provided by spec, not simulated. |
| **AC-03** Kali arm64 virtualize | **PASS** | `task-7-b2b-report.md` §3.1 (`logs/11`): built and published in **58 s**; `hypervisor=true`, `-accel hvf -cpu host -machine virt`, `uname -m` = `aarch64`; the published image is a stopped clone with one non-removable drive (no CD inherited). |
| **AC-04** ISO → install → build → reusable launch | **arm64 lifecycle PASS / x86_64 launch FAILED in the run → re-run pending / Windows UNVERIFIED** | `task-7-b2b-report.md` §4 (`logs/23`, `25`, `32`, `37`, `44`): arm64 launch → Ready in 27 s → ssh → workspace → stop (2 s) → resume (24 s) → gui → ssh all pass. x86_64 `launch` failed **3 of 3** (`logs/34`, `40`) on bugs 21/22, and one arm64 launch hit the same race (`logs/51`). Both are fixed in `7642e36` but **not yet re-run on a guest**: _(re-run pending — to be filled from task-7-rerun-report.md)_ |
| **AC-05** clone a UTM VM, original preserved | **PASS** | `task-7-b2b-report.md` §5 (`logs/47`): `profile create t7b-kali-arm64 --from-vm <uuid>` cloned an installed guest, the source VM survived and stayed `stopped`, and the clone then built and published in 41 s (`logs/48`). B2c re-confirmed the copy path (`logs/04`, `10`): two copies of one source, source unchanged. |
| **AC-06** add a user script → rebuild → verify | **FAILED in the run → re-run pending; Windows UNVERIFIED** | `task-7-b2b-report.md` §6 (`logs/49`): the custom script ran and both new `tools.verify` entries (`ltrace`, `strace`) passed in the guest, but `publish` died on bug 24 — no profile that already had an image could be rebuilt. Fixed in `6db2e92` (per-build image name, proven at the UTM level and against a fake that reproduces UTM's refusal) but **no real rebuild has run since**: _(re-run pending — to be filled from task-7-rerun-report.md)_ |
| **AC-07** per-OS commands, paths, hashes, workspace | **PASS** | `task-7-b2b-report.md` §4.3 (`logs/26`, `27`): the guest's `sha256sum`, the `.wargame-sha256` sidecar, `.wargame.json`'s `download_sha256` and the host staging hash are all `03cb4114…c1c8`; `incoming/work/notes/output` present; extraction happened in the guest only; nothing was auto-executed. |
| **AC-08** mixed backends + lifecycle after config change | **PARTIAL** | `task-7-b2b-report.md` §7 (`logs/42`, `43`): `sessions` renders UTM and Tart rows together with the right backend/mode/network, and `prune --dry-run` is correct on every point — neither installation VM nor either image is ever deletable, the running owned session is not listed, and **another home's** Tart VM is reported as ignored with a manual command rather than touched. **The TUI half is not verified**: `wargame` refuses to start without a terminal ("wargame's TUI needs an interactive terminal"), so it cannot be rendered non-interactively, and no pty driver was run this phase. |
| **AC-09** failed build preserves the previous image | **PARTIAL** | `task-7-b2b-report.md` §8 (`logs/50`, `52`): a fault-injected `tools.verify` entry failed the build **at `Verifying`**, naming the command and exit 127; the record kept the previous ready image, `profiles` flipped to `rebuild needed`, and the preserved image is still launchable and **provably the older one** (`ltrace`/`strace` absent). The other half — "a corrected rebuild then succeeds" — could not run: it is blocked by bug 24, fixed in `6db2e92`: _(re-run pending — to be filled from task-7-rerun-report.md)_ |
| **AC-10** isolated blocks, NAT is explicit | **PASS** | `task-7-b2b-report.md` §9 (`logs/38`, `41`). Isolated, from inside the guest: no default route, `getent hosts` fails, `curl 1.1.1.1` → exit 7, `curl https://kali.download` → could not resolve, the host's own LAN "Network is unreachable", while the host↔guest bridge (`192.168.128.1`) answers and SSH works throughout. This is what upgraded `doctor`'s `network:isolated:utm` from "mechanism verified, guest isolation unverified" to verified. NAT (explicitly chosen, `-y`): default route present, `http://1.1.1.1` → 301, `https://kali.download/…` → 200. No silent fallback was ever observed in either direction. |
| **AC-11** archive escape / injection prevention | **PASS** | `task-7-b2b-report.md` §10 (`logs/39`): a `../` zip, a `../` tar and a symlink tar were each refused **by name with a reason**, left packed in `incoming/`, no destination directory created, the reasons written to `notes/archive-refusals.txt`, and no sentinel file anywhere under `/tmp` or `$HOME`. |
| **AC-12** Nix build/fmt/lint/tests, no UTM needed | **PASS** | Three green gates on this branch: at `eadc3ba`, `cargo fmt --all --check` + `clippy --workspace --all-targets -D warnings` + `cargo test --workspace` → **497 passed, 0 failed, 1 ignored**, and `nix flake check` ✅ fmt/clippy/build/tests (`task-7-ac01-report.md` §4); at `14e799c`, **584 passed, 0 failed, 5 ignored** (the opt-in e2e tests), `nix flake check` ✅, `shellcheck scripts/*.sh profiles/scripts/*/*.sh` rc=0, `sacho fmt && sacho check` clean (`task-7-b2c-report.md` §1); at `4b4d825`, the same gate green with **589 tests**. Plain `cargo test` reaches neither UTM nor Tart — everything runs on `FakeRunner` and the in-memory fakes, and the real-VM tests are `#[ignore]`d *and* env-gated. |

**"Re-run pending" is not a pass.** Those three rows were observed failing on real hardware; the fixes
are covered by unit and fixture tests (including fixtures built from the exact live captures that
showed the defect) but no guest has exercised them since. They stay open until a re-run says otherwise.

### Windows, end to end: unverified

No Windows row anywhere in this record is verified. What exists is a decision and a printed manual step,
not evidence:

- UTM 4.7.5's scripting dictionary has **no TPM and no Secure Boot** — `{tpm:true}` is rejected with
  `-1700` and the sdef has zero real hits for `tpm`/`secure boot` (`task-1-report.md`, concern 1). The
  spec forbids implementing requirement bypasses, so the adopted route is a **one-time manual toggle**:
  `profile create` for `os: windows` prints, as a required step, *"Windows 11 needs TPM 2.0 and Secure
  Boot, and UTM 4.7.5 cannot set them from a script (docs/utm-compatibility.md §6). Before installing,
  open the VM in UTM > Settings > QEMU and enable "UEFI Boot", "TPM 2.0 Device" and "Reset UEFI
  Variables"."* (`crates/wargame-core/src/app/profile_build.rs`). A `duplicate` then inherits the
  configuration. The `Autounattend.xml` and `LabConfig` routes were deliberately **not** implemented.
- `guest/bootstrap.ps1` and the PowerShell transport have **never executed against a Windows guest**
  (`task-5-report.md` concern; `task-7-e2e-report.md`). `scripts/e2e-multi-os.sh` reports its three
  guest-side checks as `UNVERIFIED` for a `powershell` guest rather than guessing.
- `windows-x86_64.yaml` ships `tools.verify: []`, so the Windows cmdlet verify lines that Task 5's
  parameter-binder fix targets have never been exercised either.

## Bugs found by the real runs, and how each was fixed

Numbering continues from the MVP record above (1–13).

14. **`prune` would delete VMs another `WARGAME_HOME` owns, running or not.** `prune --dry-run` in the
    user's default home offered to delete `wg-build-t7-kali-arm64` and `wg-build-t7-kali-x86_64` — two
    *actively installing* UTM VMs owned by build records in the t7 home — because the known-set is
    per-home while the backends are host-global, and there was no running-state guard
    (`task-7-ac01-report.md`, defect 2; a real `prune` was never run). Fixed in `33f4d48`: a VM is a
    candidate only when a session record **in this home** owns it by `(backend, id)` or by the
    `<prefix><session id>` a crashed launch would have used, the record no longer justifies it
    (`Failed`/`Deleting`/`Deleted`), the session is not `keep`-flagged, and the backend reports it
    `Stopped`. Everything else is *reported* — running leftovers with the session to delete, unmanaged
    VMs with the manual command for their backend — never deleted. The Tart provider gained the
    namespace floor UTM already had. `d2bbd6c` then made the advertised recovery work (`delete` can
    finish an interrupted delete and clean up a session stuck mid-launch) and settled records after a
    prune. Help text corrected (`33f4d48`), README's ownership rule added (`8adc85c`).
15. **`scripts/e2e-smoke.sh` was broken and leaked a running VM when it broke.** It parsed
    `^session id: `, a line the 2026-09-08 usability pass (`eb5a329`, on `main`) had removed, and failed
    *after* the VM was up but *before* `SESSION_ID` was set, so its `EXIT` trap deleted nothing
    (`task-7-ac01-report.md`, defect 1 — one leaked VM was removed by hand). Fixed in `1320bac`: the id
    comes from `wargame sessions --json`, and the trap is armed off a snapshot of the session ids that
    existed *before* the launch, so cleanup no longer depends on any parse and pre-existing sessions
    survive.
16. **The printed setup guidance omitted what the build hard-requires.** `profile create` asked for the
    user, `sshd`, the guest agent and the key, but never for passwordless sudo — which
    `profiles/scripts/kali/common.sh` aborts on in its first check, so a user following the instructions
    exactly gets a build that dies on step 1 of provisioning (`task-7-phase-a-report.md` D2). Fixed in
    `eadc3ba`. `b06c46d` fixed the neighbouring wording: check the agent with `systemctl is-active`, not
    `is-enabled`, because Kali's unit is udev-activated and correctly answers `static`; and it added the
    line that an emulated guest uses every core it is given, so two builds at once make both crawl.
17. **The free-space check refused a create it should have allowed, and measured the wrong volume.** It
    compared the profile's *nominal* `disk_gib` (80 for both shipped Kali templates) against
    `$WARGAME_HOME`'s volume, so the shipped profiles could not be created at all on any host with less
    than 80 GiB free — for a sparse qcow2 that starts at `host size = 0` and holds a finished minimal
    Kali in 2.3 GiB — and it pointed the user at `wargame sessions` (`task-7-phase-a-report.md` D1).
    Fixed in `a6bcd2b`: a documented `MIN_FREE_GIB = 8` floor, a new `VmProvider::storage_dir()` so the
    UTM adapter measures `~/Library/Containers/com.utmapp.UTM/Data/Documents` (falling back to the home
    if the sandboxed directory cannot be read), and an `InsufficientResources { detail, hint }` that
    explains sparseness instead of suggesting a lower `disk_gib`. `doctor` still used the old check
    until `14e799c`, which routes it through the same `free_space_for`/floor — a ✓ where the acceptance
    run got a ✗ (`task-7-b2c-report.md` `logs/11`).
18. **Nothing ejected the installer ISO.** `profile create` attaches the ISO as drive 1 and UEFI boots
    it first, so after the install the VM came straight back to the installer: `profile console` would
    land in it, `profile build` would burn its whole boot timeout, and `publish` would clone the CD into
    the image. Phase A detached it by hand with AppleScript (`task-7-phase-a-report.md` D3). Fixed in
    `94d578e`: an `eject_installer_media` step between `clone_for_build` and `boot`, plus a
    `wargame profile eject` verb; the drive list is written back without its removable entries, echoing
    each survivor's own `id` (or UTM adds a drive instead of keeping one) and only on a `stopped` VM.
    `BuildRecord` gained `installer_media: Unknown | Attached | Detached`. A review of that fix found a
    latent data-loss path — `removable` cannot be changed after a drive is created, so a `--from-vm`
    VM can have its *boot disk* flagged removable and the eject would have detached the user's installed
    system — fixed in `0d9f44b`: the script refuses before either write when no drive would survive, and
    a build from an adopted VM ejects nothing at all.
19. **There was no CLI verb for the installation VM.** `profile console` starts it and nothing could
    stop, eject or delete it; every such action in phase A went through `utmctl` by hand
    (`task-7-phase-a-report.md` D4). Fixed in `ce18a55`: `profile stop` (the same `VmProvider::stop`
    path a session stop uses, including the T12 orphan refusal that names the pid and never signals a
    process wargame did not spawn), `profile eject`, and `profile delete [--force]` (installation VM,
    every image the profile published, `leaked_images`, then the record last, refusing while any of them
    is running).
20. **CRITICAL — `delete` removed every UTM virtual machine on the host.** `vm_script` wrapped each verb
    in `repeat with vm in (virtual machines)` and applied the verb to the loop variable, which binds a
    reference *into* the collection: `delete vm` deleted the whole library and still returned `"ok"`.
    Reproduced minimally with three disposable VMs — one UUID passed in, `utmctl list` empty afterwards
    (`task-7-b2b-report.md` §2 D1, `logs/65`); a *running* VM raises `-2700` and aborts the cascade
    partway, which is why the damage first looked like an ordinary error (`logs/66`).
    **What it destroyed: the verification environment.** `wargame delete <session> --force` took phase
    A's hand-installed arm64 guest and its image; `wargame profile delete t7-eject --force` — the
    throwaway profile the brief named — took **all eleven** remaining UTM VMs, including phase A's
    x86_64 guest, while reporting that it had deleted only `wg-build-t7-eject`. Free space rose
    18 → 25 GiB, so it was a real on-disk deletion. **No VM of the user's was affected and Tart was
    never touched**: `kindle-vm`, both `wargame-profile-macos-*` images and the user's session VM
    `wg-dh-sprout-base64-3edf2c` are byte-identical before and after, and the host had no UTM VM at all
    before this branch (`task-7-b2b-report.md` §13, "End state"). Fixed in `7642e36`: every verb and
    every `update configuration` addresses `(first virtual machine whose id is vmId)`; the `repeat` loop
    survives only as the existence check behind the `wargame-utm:vm-not-found` sentinel. Two audit tests
    enumerate all fifteen script templates and fail if any applies a verb to the loop reference or omits
    the by-id specifier, and `4b4d825` made the enumeration self-checking by scanning this module's own
    source, so a new template cannot escape the audit silently. **Proven live** on real VMs: the exact
    command from the disaster, with a *running* bystander in the list, went 6 rows to 5 and removed
    exactly the VM it named (`task-7-b2c-report.md` §2 D1, `logs/08`); `start`, `stop`, `duplicate`,
    `set_network_mode` and the eject write branch each affected exactly one VM (`logs/03`–`07`); and a
    gated e2e test makes three VMs, deletes the middle one and requires the registry to differ by
    exactly that UUID (`logs/09`).
21. **A bare IPv6 literal was handed to `scp`.** `copy_in` built `user@host:dest`, and `scp` splits on
    the first colon, so `kali@fd14:a432:…:299:/path` became host `kali@fd14`. `ssh` accepts the bare
    literal, so the readiness gate passed and the failure only surfaced at the first copy — after the
    boot the user had waited for. It killed two `profile build`s and three `launch`es
    (`task-7-b2b-report.md` §2 D2, `logs/15`). Fixed in `7642e36`: a separate `copy_target` brackets an
    IPv6 literal (`user@[addr]`) while `ssh`'s target is untouched, with a test that pins the IPv4 form
    byte-identical.
22. **The address gate accepted a routable IPv6 before the IPv4 lease.** `wait_ip` returned on the first
    poll that produced anything routable, and on these guests the SLAAC ULA appears ~13 s before the
    DHCPv4 lease — captured live at 11:55:02 (IPv6 only) and 11:55:15 (IPv4 joined)
    (`task-7-b2b-report.md` §2 D3). Fixed in `7642e36`: `guest_addresses` returns every usable address
    and `wait_ip` returns only on IPv4, polling on within the same boot timeout; a timeout with IPv6
    only says so and names what it saw. Both live polls are committed as fixtures.
23. **Every clone inherited the image's MAC, so two sessions collided on one IP.** UTM's `duplicate`
    copies the network interface, and nothing re-randomised it: two sessions from one image came up on
    the same segment with the same MAC *and* the same address, and an `scp` addressed to that IP landed
    in the **other** session's VM (`task-7-b2b-report.md` §2 D4, `logs/28`). Fixed in `7642e36`: a
    partial interface record writes a fresh locally-administered unicast MAC onto every copy while it is
    still stopped, leaving mode, hardware and port forwards alone. Proven live — three copies of one
    source got three different addresses and the source kept its own (`task-7-b2c-report.md` `logs/04`,
    `10`).
24. **`publish` collided on the UTM bundle name, so no profile could be rebuilt.** A VM's display name
    *is* its bundle name; `duplicate` copies the bundle, registers the copy, applies the requested name
    and only then moves the bundle into place, so a second image under the same name fails that move
    with `-2700` — **after** a full successful provision and verify — and still leaves a registered VM
    the record knew nothing about (`task-7-b2b-report.md` §2 D5, `logs/49`; mechanism investigated live
    and written up as `docs/utm-compatibility.md` T14). Fixed in `6db2e92`: publish under
    `wg-build-image-<profile>-<6 hex>`, a name nothing holds, keeping FR-3's "the previous image lives
    until the record names the new one"; a new `ImageRegistrar` port records **every** VM UTM registered
    — including the one a failed `duplicate` leaves — into `leaked_images` and saves the record *before*
    the error is raised, so `prune` skips it and the next successful build removes it. `4b4d825` moved
    that report to before the copy's MAC is rewritten, closing the crash window, and deletes a copy this
    app cannot finish configuring.
25. **`doctor` and `profiles` disagreed with reality.** Three separate holes, all fixed in `14e799c`:
    an image the record named but UTM no longer had still read `ready` (now the backend is asked too,
    and the state word `missing` exists — demonstrated live by deleting a VM with `utmctl` behind
    wargame's back, `task-7-b2c-report.md` `logs/12`); `doctor --profile` used the pre-fix disk check
    (bug 17); and the "not set up yet" hint pointed at `profile create` for a profile whose installation
    VM already exists, the one command that would refuse — it now points at `profile console`. A review
    round added the fourth case in `4b4d825`: a backend that *cannot answer* (denied Automation, an app
    that will not launch) was collapsed into `missing` plus "rebuild it"; it now reads `unknown` and
    carries the backend's own message and `wargame doctor` as the hint.
26. **`wargame gui` silently rebooted a running session's guest.** It took 22 s, printed "SSH keeps
    working", and the guest came back with `uptime` = 0 min — in-guest state lost without warning
    (`task-7-b2b-report.md` §2 D8, `logs/44`). Fixed in `14e799c`: `VmProvider::show_console` (default
    `false`); UTM answers `true` after bringing the app to the front, because it has **no headless start
    at all**, so a running VM's window was open the whole time and only needed focusing. Tart keeps the
    stop/start its headless runner needs.

Two further findings were recorded rather than changed. **Isolated guests have no DNS by design**: the
`vmnet-host` lease carries the host's LAN resolvers, which are on the far side of the isolation
boundary, so lookups fail by timeout rather than by configuration; rewriting the guest's `resolv.conf`
would be an invisible network change in the one mode whose value is that nothing reaches out
(`docs/utm-compatibility.md` §5). And **UTM's own input automation is an exit-0 no-op on this host** —
`input keystroke` and `input scan code` returned success and produced zero guest reaction in five
variations, against a live serial console that was simultaneously showing GRUB output — so
`docs/utm-compatibility.md` §2 downgrades that row from verified to *unverified/unreliable, do not build
on it* (`task-7-phase-a-report.md` D6, `b06c46d`).

## Timings worth keeping

| | Kali arm64 — `virtualize` (HVF) | Kali x86_64 — `emulate` (TCG) |
|---|---|---|
| Unattended ISO install, boot → `finish-install` | ~4 810 s (**1 h 20 m**) | ~36 000 s (**10 h**) |
| Power-on → guest-agent address (installed guest) | 23 s | 44–54 s |
| Power-on → SSH ready (installed guest) | **25 s** | **48–68 s** |
| Whole `wargame profile build` | **58 s** | **257 s** |
| — provisioning step alone | ~20 s (197 MB at 55.5 MB/s) | ~180 s (221 MB at 17.7 MB/s) |
| `launch` → Ready (session from the image) | 27 s | not reached (bugs 21/22) |
| `stop` / `resume` / `gui` | 2 s / 24 s / 22 s | — |

Both install figures are **pessimistic and starved**: the two installs ran concurrently on a 10-core
host with 4 vCPU each, one of them TCG; both guests logged `soft lockup` warnings and the host load
average peaked around 24. Run installs sequentially, or lower `resources.cpu`.

**The 1800 s `emulate` boot timeout is right and generous** — roughly 26× the measured boot; the 300 s
`virtualize` default has ~12× headroom. Phase A's worry that provisioning under TCG would be the long
pole did not materialise (221 MB of packages in about three minutes). Neither number needs changing; the
thing that actually broke these paths was bugs 21/22, which no timeout can fix.

## What is verified on real VMs, and what only fakes have seen

| Exercised on real hardware | Only exercised against fakes / fixtures |
|---|---|
| Both macOS profiles end to end on real Tart VMs, incl. the pre-branch session record (AC-01) | `copy_in`'s IPv6 bracketing (bug 21) — a unit test, no guest |
| Kali arm64 and x86_64 installed from ISO, `profile build`, publish, `uname -m`, `hypervisor` off (AC-02, AC-03) | `wait_ip`'s IPv4 wait (bug 22) — two fixtures carrying the exact live polls, no guest agent |
| `profile create --iso` and `--from-vm`, `profile console`, source preservation (AC-05) | The rebuild/publish flow (bug 24) — a fake registry that reproduces UTM's `-2700`; no real second build |
| `profile eject`: the no-op path, the read path, and the write branch on a throwaway VM with an ISO attached | `show_console` / `gui` not restarting a guest (bug 26) — a unit test, no session |
| `delete`, `start`, `stop`, `duplicate`, `set_network_mode` each affecting exactly one VM, with a running bystander present (bug 20) | `prune`'s deletion paths and record settling — 19 lifecycle tests on in-memory fakes; a real `prune` was never run |
| A clone getting its own MAC; three copies, three addresses (bug 23) | `guest.user` validation, `doctor`'s three-way Pass/Fail/NotSetUp, the `unknown` image state |
| `doctor`'s disk row on the right volume; a record's image going `missing` when UTM loses it | Everything Windows: `guest/bootstrap.ps1`, the PowerShell transport, `-EncodedCommand`, the 3010 reboot path |
| Guest-side isolation and NAT, from inside the guest (AC-10) | `scripts/e2e-multi-os.sh` stages 9–12 (guest SSH, `uname -m`, workspace, hashes, stop/resume/delete, source preservation) — a full `WARGAME_VM=fake` dry run proved control flow and cleanup only |
| Guest file hashes against host staging (AC-07); archive-escape refusals in the guest (AC-11) | `crates/wargame-utm/tests/e2e.rs` beyond compiling, skipping and the delete test |
| The gate: fmt, clippy, `cargo test --workspace`, `nix flake check`, `shellcheck`, `sacho check` (AC-12) | Tart's "previous image preserved on failure" exception (its `prepare` deletes the image before cloning) |

## Explicitly unverified

- **Windows, end to end.** No ISO on this host; see the Windows subsection above. The TPM/Secure Boot
  manual step is a printed instruction nobody has followed on this host.
- **AC-04 (x86_64), AC-06 and AC-09's corrected-rebuild half after the fixes.**
  _(re-run pending — to be filled from task-7-rerun-report.md)_
- **Bugs 21, 22 and 26 live.** All three are covered by tests built from the live captures that showed
  them, and none has met a guest since the fix.
- **The eject write branch on a VM with an installed guest.** It is proven live on a throwaway VM
  created from an ISO (drives 2 → 1, the non-removable drive surviving, every other VM untouched), but
  not yet on a VM whose surviving disk holds an OS.
  _(re-run pending — to be filled from task-7-rerun-report.md)_
- **The TUI on a multi-OS session.** It refuses to render non-interactively; no pty driver was run
  (AC-08).
- **`STATUS_BODY`** is the one AppleScript template still reading through the loop variable (it reads
  two properties and acts on nothing); the by-id form for it has not been run against real UTM. The
  `suspend` verb has no adapter body at all.
- **`SHA256SUMS.gpg`** signature verification for downloaded ISOs.
- **A real `wargame prune` deletion** on a UTM host, and `delete`/`prune` with an installation VM and an
  image present throughout.
- **UTM's `unavailable` backend state**, `--disposable`, `--recovery`, `stop --request`, guest file
  push/pull and USB attach — dictionary entries never exercised (`task-1-report.md`).

## The ISO installs, as a separate manual step

The spec allows the first OS install to be manual and asks for it to be recorded with its own evidence.
Full transcript, serial captures and the preseed file: **`.superpowers/sdd/PLAN/task-7-phase-a-report.md`**
(raw logs were session scratch and are gone; the report quotes them).

Method: **unattended Debian preseed**, with the boot line delivered over the UTM serial ptty. No human
clicked anything — which mattered, because the graphical console was unreachable to the installing agent
(no Screen Recording grant, and UTM's input automation is a no-op here). What it took:

- The serial ptty comes up with `echo` on, so QEMU's own output is typed back into the guest as input
  (GRUB pressing its own Enter). The reader must open the device **first**, then `stty -f "$SER" raw
  -echo` — opening a pty slave nobody holds resets its termios.
- The GRUB command line (`c`) takes a fresh `linux`/`initrd` pair with
  `auto=true priority=critical preseed/url=http://<host>:8642/preseed.cfg … console=ttyAMA0,115200`,
  served by `python3 -m http.server` on the host's address **on the `vmnet-shared` bridge** (not
  `10.0.2.2`, which only exists in UTM's `emulated` mode).
- amd64 only: the ISO's `grub.cfg` switches to `gfxterm`, so its menu is invisible on the serial line.
  A scratch ISO with a two-line `grub.cfg` replacement was built with `xorriso`; the VM was pointed back
  at the pristine ISO afterwards, and both original ISOs' hashes were re-verified unchanged.
- The preseed pins `kali.download` (not `http.kali.org`, whose HTTPS redirect fails against a target
  system with no CA bundle yet), installs no desktop, forces
  `grub-installer/force-efi-extra-removable`, and its `late_command` writes
  `/etc/sudoers.d/wargame` (passwordless sudo for `kali`), the host's public key into
  `~kali/.ssh/authorized_keys`, and enables `ssh` + `qemu-guest-agent`.
- After the install the CD had to be detached by hand — the gap that became bug 18 and
  `wargame profile eject`.

Both guests were then verified live: `utmctl status started`, an address from the guest agent, key-based
SSH as `kali` with `BatchMode=yes`, `sudo -n true` exit 0, `sshd` enabled and active, the guest agent
active, `uname -m` = `aarch64` / `x86_64`, `/etc/os-release` = Kali 2026.2, and `apt-get update` exit 0
on both. **Both of those installs were destroyed by bug 20** (see above) and are being installed again;
the re-run inherits that work.
