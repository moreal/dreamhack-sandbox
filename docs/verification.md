# Verification record — 2026-09-05 / 2026-09-06

Host: macOS 26.6.2, Apple Silicon, Tart 2.36.0 (via nixpkgs wrapper) / 2.32.1 (Homebrew), Determinate Nix 3.21.9.
Branch: `feat/mvp`.

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

### Bugs found and fixed by the real Dreamhack run (uncommitted at the time of writing)

6. The adapter treated the download response's top-level JSON string as `ProviderChanged`; only object shapes were accepted → `file_refs_from_json` accepts the bare string first, object/`files[]` shapes stay as fallbacks.
7. Dreamhack's CSRF cookie is `csrf_token`, not Django's default `csrftoken`, so no `X-CSRFToken` was sent and the 403 was reported as "authentication required" → `client::csrf_token` tries `csrf_token` then `csrftoken` (login and download); a 403 whose body says `CSRF Failed` is now reported as `ProviderChanged` with the cookie names tried, so it cannot be mistaken for a missing login.
8. Object storage names the file `<uuid>.zip` via `Content-Disposition` → `choose_filename` prefers the asset placeholder (`<repository>.zip`) over a UUID-stem name when the extensions match, so the guest sees `rev-basic-1.zip`.

## Not verified live

- `wargame gui` (re-runs the VM with a Tart window) was only covered by unit tests; it would open a window on the host.
- `macos-re-arm64` (Ghidra) image build was not run; it uses the same builder with `reversing.sh` on top of the verified `common.sh`.
- Isolated networking end to end, because it requires the sudoers rule on this host.
