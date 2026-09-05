# Verification record — 2026-09-05

Host: macOS 26.6.2, Apple Silicon, Tart 2.36.0 (via nixpkgs wrapper) / 2.32.1 (Homebrew), Determinate Nix 3.21.9.
Branch: `feat/mvp`.

## Build and static checks

| Check | Command | Result |
|---|---|---|
| Format | `nix develop -c cargo fmt --all --check` | clean |
| Lints | `nix develop -c cargo clippy --workspace --all-targets -- -D warnings` | clean |
| Unit + fixture + CLI tests | `nix develop -c cargo test --workspace` | 203 passed, 1 ignored (the real-VM e2e), 0 failed |
| Reproducible build | `nix build` | `result/bin/wargame` (wrapped with `tart`, `ssh`, `sshpass` on PATH) |
| Flake checks | `nix flake check` | see "Flake check" below |

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

See the section appended below once the run completes.
