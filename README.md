# wargame-vm

`wargame` is a CLI/TUI that searches [Dreamhack](https://dreamhack.io)
wargame challenges, launches a disposable macOS VM ([Tart](https://tart.run/),
Apple Silicon only) from a declarative profile, copies the challenge's files
into the guest without ever unpacking or running them on your host, and
manages the resulting session's lifecycle (stop / resume / keep / delete).

Every VM is a throwaway clone of a profile image: solve the challenge inside
it, then discard the clone. The base profile image is never mutated.

Built as a Nix flake so `nix run`, `nix build`, `nix develop`, and `nix flake
check` are the only entry points anyone needs — no separate Rust toolchain
install.

## Requirements

- macOS on Apple Silicon (`aarch64-darwin`). The core crates and their unit
  tests also build and pass on `x86_64-linux` (that's what keeps `nix flake
  check` portable in CI), but the VM backend and every end-to-end flow are
  macOS/Tart-only.
- [Tart](https://tart.run/) (fetched automatically by the Nix package below;
  it's Fair Source, not open source, which is why the flake has to
  explicitly allow it).
- [Nix](https://nixos.org/download) with flakes enabled.

## Install / run

```sh
# Run without installing anything:
nix run github:moreal/dreamhack-sandbox

# Or build a local binary at ./result/bin/wargame:
nix build github:moreal/dreamhack-sandbox
./result/bin/wargame --help

# From a checkout of this repo, either of the above works with `.` in
# place of the github: URL, e.g.:
nix run .
nix build .
```

`nix build`/`nix run` wrap the `wargame` binary so `tart`, `ssh`, `scp`,
`ssh-keygen`, and (on Darwin) `sshpass` are found on `PATH` at run time even
if you don't have them installed globally — see [Nix layout](#nix-layout).

### Development shell

```sh
nix develop
cargo build --workspace
cargo test --workspace
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
```

`nix develop` provides `rustc`/`cargo`/`clippy`/`rustfmt`/`rust-analyzer`,
`cargo-nextest`, `jq`, `shellcheck`, `openssh`, and (on Darwin) `tart` and
`sshpass`. Never install the Rust toolchain yourself — everything here runs
through this shell.

## First run

```sh
$ wargame doctor
```

`doctor` never fails outright — it prints a full, independent list of
checks (platform, Tart availability, `ssh`/`scp`/`ssh-keygen`/`security` on
`PATH`, disk space, `$WARGAME_HOME` writability, each profile's image,
Dreamhack reachability + auth state, the guest SSH key) so you know exactly
what's missing before doing anything else. Add `--json` for scripting.

```sh
$ wargame auth login
Email: you@example.com
Password: ********
Logged in as you@example.com.
```

Your Dreamhack session cookie is stored in the macOS Keychain (service
`wargame-vm`) — never your password, and never in a config file. See
[Security model](#security-model).

```sh
$ wargame profiles
NAME                  DESCRIPTION
macos-minimal-arm64   macOS minimal: shell utilities, Git, Python/uv
macos-re-arm64        macOS reversing: Ghidra, LLVM, Python/uv

$ wargame profile build macos-re-arm64
```

`profile build` clones the (large, one-time-downloaded) vanilla Tart base
image, boots it, generates a per-install ed25519 key under
`$WARGAME_HOME/keys/` the first time it's needed, installs that key's public
half into the clone over a temporary password login, provisions it with the
profile's scripts (`profiles/scripts/macos/*.sh`), runs the profile's
`tools.verify` commands, then stops it and keeps the result as a named local
Tart image (`wargame-profile-<name>`). This step is slow (tens of minutes)
and is the only place a guest password is ever used — every VM launched
afterwards authenticates with the key alone.

```sh
$ wargame search out_of_bound
$ wargame open dreamhack:11
$ wargame launch dreamhack:11 --profile macos-re-arm64 --network isolated
… downloading out_of_bound.zip
✓ downloaded out_of_bound.zip (…)
… booting
✓ guest ip: 192.168.…
… bootstrapping
✓ Ready: session dh-out-of-bound-a1b2c3 (wargame ssh dh-out-of-bound-a1b2c3)

$ wargame ssh dh-out-of-bound-a1b2c3
```

`launch` clones the built profile image, boots it, streams the challenge's
files to a local staging directory (hashing them as they arrive), copies
them into the guest as opaque bytes, and runs the guest-side bootstrap
script — which lays out `incoming/ work/ notes/ output/` and, unless
`--no-extract` is passed, unpacks known archive types (zip/tar) *inside the
guest*. Nothing is ever extracted, previewed, or executed on the host.

When you're done:

```sh
$ wargame sessions
NAME                  PROBLEM        PROFILE          STATE
dh-out-of-bound-a12   out_of_bound   macos-re-arm64   running

$ wargame delete dh-out-of-bound-a12          # discard the clone
$ wargame keep dh-out-of-bound-a12            # or: mark it to survive `prune`
$ wargame stop dh-out-of-bound-a12            # or just stop it for now
$ wargame resume dh-out-of-bound-a12          # ...and pick it back up later
```

Running `wargame` with no subcommand opens the TUI, which drives the same
application services (search → problem detail → launch → sessions) as the
CLI above — there is no TUI-only business logic.

## What "isolated" networking means

`wargame launch` defaults to `--network isolated`. Concretely, that's
`tart run --net-host`: the guest is given Tart's host-only vmnet interface,
which can reach the Mac's own network stack (so SSH from the host still
works) but cannot reach the LAN or the internet. This is the right default
for offline reversing/file-analysis challenges — plausible malware inside a
challenge archive has no network to call out on.

`--network nat` gives the guest Tart's normal shared networking (outbound
internet access) for challenges that need it — remote challenge servers,
package installs, web problems. Because that materially changes the threat
model, `wargame` always shows an explicit warning and asks
`Continue? [y/N]` before launching with NAT, unless you pass `--yes`:

```
Warning: NAT networking connects this VM directly to your network, rather
than isolating it (MVP.md §18).
Continue? [y/N]
```

Neither mode implements fine-grained egress filtering (e.g. blocking
specific LAN hosts while allowing the internet) — that's future scope, not
in this MVP.

## Security model

- **No host-side extraction.** Challenge files are copied into the guest as
  opaque bytes and hashed (SHA-256) along the way; only the disposable guest
  ever unpacks or runs anything from them, and only when `--extract` (the
  default) is set. The host never mounts, previews, or executes a challenge
  artifact.
- **Ephemeral by default.** Every launch clones the profile's base image
  fresh; nothing about a session mutates the canonical profile image, and a
  session with no `keep` flag is easy to delete completely (`wargame delete`,
  or `wargame prune` for orphaned VMs Tart still has on disk).
- **Isolated networking by default**, with an explicit, confirmed opt-in for
  NAT — see above.
- **No secret ever touches argv, logs, or a config/session file.** Every
  subprocess is invoked as `Command::new(bin).args([...])`, never a shell
  string, and process argv is what gets logged — so secrets are kept out of
  argv by construction, not by redaction alone. Where a value must reach a
  subprocess and can't go through argv (a Keychain password, a guest build
  password) it goes through stdin or an environment variable set only on
  that one child process, never printed. Tracing output additionally passes
  through a redaction layer that masks known secret shapes (`Bearer …`,
  `sessionid=…`, `csrftoken=…`, `Cookie:`/`Authorization:` header values,
  private key blocks) as defense in depth.
- **Credentials live in the macOS Keychain** (service `wargame-vm`), written
  and read through `/usr/bin/security -i` (a value passed over stdin, never
  as an argv `-w` argument with the secret inline). Only the resulting
  Dreamhack session cookie is persisted — your password is never stored,
  only used once at login time. A plaintext `FileStore` exists behind the
  same trait for tests/CI (`credential_store = "file"` in config) and warns
  loudly when constructed; don't use it for a real account.
- **The guest SSH key is per-install**, generated with
  `ssh-keygen -t ed25519 -N ''` into `$WARGAME_HOME/keys/` (mode 0600) the
  first time a profile is built, authorized only inside that profile's
  images, and never leaves the host except as its public half.
- **Guest-side archive extraction only**, and only for a small allow-listed
  set of formats (zip, tar/tar.gz/tgz) via the guest's own `unzip`/`tar`
  (which both refuse to write outside the destination directory they're
  given); the bootstrap script additionally scopes every archive to its own
  `incoming/extracted/<name>/` subdirectory and never executes anything it
  finds — extraction only, never execution.
- **Every user-facing error carries an explicit hint** (e.g. `ImageMissing`
  → "Run: wargame profile build <profile>"), so failure modes are
  actionable rather than opaque.

See `DESIGN.md` §3.6 for the exact mechanisms and `MVP.md` §18/§37 for the
full threat model this implements against.

## Troubleshooting

Run `wargame doctor` first — it's the fastest way to see exactly which
prerequisite is missing, with a `hint` for each failing check.

- **`tart not found` / `vm-backend:tart` fails** — install Tart (the Nix
  package wraps it in for you on Darwin; otherwise `brew install cirruslabs/cli/tart`)
  and make sure it's on `PATH` inside whatever shell you're running
  `wargame` from.
- **`profile-image:<name>` fails** — that profile hasn't been built yet; run
  `wargame profile build <name>` (slow the first time — it pulls the base
  image and provisions from scratch).
- **`ImageMissing` on `wargame launch`** — same fix as above; the error's
  own hint says so.
- **Guest boot times out** — check `$WARGAME_HOME/logs/wargame.log` and the
  session's `run.log` under `$WARGAME_HOME/sessions/<id>/`; a cold Tart
  image can take longer than the configured `boot_timeout_secs`
  (`[vm.tart]` in `config.toml`) to get a DHCP lease on some hosts.
- **`provider:dreamhack` unreachable / `AuthenticationRequired`** — check
  network access to `https://dreamhack.io`, then `wargame auth login`
  again; `wargame auth status` shows the current state without touching
  anything.
- **`sshpass` missing** — only needed for `wargame profile build` (the
  one-time password-based key install); everyday `launch`/`ssh` never uses
  it.
- **Config file** lives at `$WARGAME_HOME/config/config.toml` (all fields
  optional; see `DESIGN.md` §3.5 for the schema) — `$WARGAME_HOME` defaults
  to `~/Library/Application Support/wargame-vm` and can be overridden with
  the `WARGAME_HOME` env var or `--home` for a fully hermetic setup (this is
  exactly what `scripts/e2e-smoke.sh` and the test suite do).

## Testing

```sh
nix develop --command bash -c \
  'cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace'
```

runs everything that doesn't need a real Tart VM or a Dreamhack account —
domain/state-machine tests, Dreamhack fixture parsing, Tart argv
construction against a fake process runner, and CLI tests run with
`WARGAME_PROVIDER=fixture WARGAME_VM=fake` (no network, no VM). This is also
what `nix flake check` runs.

Two more layers exist for real hardware, both documented rather than run in
CI:

- **`scripts/e2e-smoke.sh [profile-name]`** — drives the *real* `wargame`
  binary against a *real* Tart VM (still with `WARGAME_PROVIDER=fixture`, so
  no Dreamhack account needed): builds, runs `doctor`, launches the fixture
  challenge into the named profile (default `macos-minimal-arm64`, which
  must already be built), verifies the guest-extracted file over SSH, then
  deletes the session. Run it from inside `nix develop`, with the named
  profile already built once (`wargame profile build macos-minimal-arm64`).
- **`cargo test -p wargame-tart --test e2e -- --ignored`** — the same M0
  proof (MVP.md §36) driven directly against `wargame-tart`, with real
  password authentication against a freshly cloned vanilla image (no
  profile build required first). Skips unless `WARGAME_E2E=1` is also set,
  so a blanket `--ignored` run never accidentally boots a macOS VM by
  accident:

  ```sh
  WARGAME_E2E=1 cargo test -p wargame-tart --test e2e -- --ignored --nocapture
  ```

## Nix layout

- `packages.default` (`wargame`) — `rustPlatform.buildRustPackage` against
  the pinned `Cargo.lock`, with `doCheck = true` (so `nix build` also runs
  `cargo test`), wrapped with `makeWrapper --prefix PATH : …` so `tart`,
  `sshpass`, and `openssh` are resolved at run time without needing them
  installed outside Nix.
- `apps.default` — `wargame` itself, for `nix run`.
- `devShells.default` — the toolchain described above.
- `checks.{build,fmt,clippy,tests}` — what `nix flake check` runs on both
  `aarch64-darwin` and `x86_64-linux` (the latter only exercises the
  cargo-portable parts: domain logic, fixtures, argv construction — never
  Tart itself).

## Project layout

```
crates/
  wargame-core/       domain types, ports (traits), application services,
                       security helpers, infra adapters, test fakes
  wargame-dreamhack/   Dreamhack provider (implements WargameProvider)
  wargame-tart/        Tart VM backend (implements VmProvider) + profile
                       image builder
  wargame-cli/         CLI (clap) + TUI (ratatui) composition root
profiles/              built-in profile YAMLs + their provisioning scripts
guest/                 bootstrap.sh, run inside the guest only
scripts/e2e-smoke.sh   manual real-VM smoke test (see Testing)
```

`wargame-dreamhack` and `wargame-tart` each depend only on `wargame-core` —
never on each other — so a challenge-source change can never leak into the
VM backend or vice versa.

See `MVP.md` for the product spec and `DESIGN.md` for the full technical
design this implements.
