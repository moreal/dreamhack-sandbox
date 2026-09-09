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
name                  guest           backend/mode     image
macos-minimal-arm64   macos/arm64     tart/virtualize  ready
macos-re-arm64        macos/arm64     tart/virtualize  ready
kali-arm64            kali/arm64      utm/virtualize   not created
kali-x86_64           kali/x86_64     utm/emulate      not created
windows-x86_64        windows/x86_64  utm/emulate      not created

$ wargame profile build macos-re-arm64
```

`backend/mode` is what the profile *resolved* to, not what it asked for: a
profile that says `backend: auto` shows the backend that will actually run
it. `wargame doctor`, the TUI's profile picker and `wargame launch`'s
preflight output show the same four answers, and `wargame sessions` /
`wargame session inspect` name the backend each existing session runs on.

The `image` column is the profile's build state: `ready` (there is an image
to launch from), `not built`/`not created`, `rebuild needed` (the profile or
one of its scripts changed since the image was built), or the phase a
half-finished build stopped at.

`profile build` clones the (large, one-time-downloaded) vanilla Tart base
image, boots it, generates a per-install ed25519 key under
`$WARGAME_HOME/keys/` the first time it's needed, installs that key's public
half into the clone over a temporary password login, provisions it with the
profile's scripts (`profiles/scripts/macos/*.sh`, embedded in the binary and
written to `$WARGAME_HOME/profiles/scripts/`), runs the profile's
`tools.verify` commands, then stops it and keeps the result as a named local
Tart image (`wargame-profile-<name>`). This step is slow (tens of minutes)
and is the only place a guest password is ever used — every VM launched
afterwards authenticates with the key alone.

### Kali and Windows profiles (UTM)

macOS guests come from a ready-made Tart base image. Kali and Windows do
not: there is no base image to clone, so the first OS installation is done
by hand, once, in a UTM window — after which every launch clones the image
that install produced.

```sh
$ wargame profile init kali-arm64 --name kali-custom     # your own copy, with its own scripts/
$ wargame profile create kali-custom --iso ~/isos/kali-arm64-installer.iso
$ wargame profile console kali-custom                    # install the OS in the window that opens
$ wargame profile build kali-custom                      # provision + verify + publish the image
$ wargame launch dreamhack:15 --profile kali-custom
```

`profile init` copies a template under your own name and gives it its own
`profiles/scripts/<name>/` directory, so your edits are never overwritten
when a new binary re-seeds the built-in scripts. `profile create` makes one
long-lived *installation VM* (a blank disk plus the installer ISO, or
`--from-vm <utm-uuid>` to clone an already-installed VM, which is only ever
copied) and prints its UUID plus what to prepare in the guest: your user
account, an SSH server, the QEMU guest agent, this host's public key in
`~/.ssh/authorized_keys`, and the elevated access the build needs — a
`NOPASSWD` sudoers entry on Kali, membership of Administrators on Windows,
since a build installing packages over a non-interactive SSH command cannot
answer a password prompt. Re-running it with the same source re-prints that
guidance and creates nothing; pointing it at a *different* source is an
error rather than a silent second VM. `--sha256 <hex>` is checked against
the ISO before anything is created.

`profile build` then boots that VM on NAT, waits for the guest agent and
SSH (30 minutes by default for an emulated x86_64 guest —
`[vm.utm].boot_timeout_secs`), runs the profile's scripts and `tools.verify`
commands, shuts the guest down cleanly, and publishes the image as a stopped
copy. Only that last step marks the profile ready: a guest that is still
being installed stays in `awaiting-install`, and a failed build keeps the
previous image launchable. The installation VM is never deleted — it is what
the next rebuild starts from — and neither it nor the published image is
touched by `wargame prune`.

Windows 11 additionally needs TPM 2.0 and Secure Boot, which UTM 4.7.5
cannot set from a script; `profile create` prints the one-time manual step
(UTM ▸ Settings ▸ QEMU) for a `windows-*` profile. You supply the Windows
ISO and its licence.

Windows guests are driven through PowerShell rather than a POSIX shell:
`profile build` copies `profiles/scripts/windows/*.ps1` into the guest and
runs each one with `powershell -File`, a script that exits 3010 makes the
build wait for the reboot and carry on (even though `ssh` itself can only
report the low byte of an exit code), `tools.verify` runs the same way,
and `wargame ssh <session>` lands you in PowerShell. Commands cross the wire
as `-EncodedCommand` (UTF-16LE + base64), so Windows paths, spaces, quotes
and Korean characters survive intact; files go over `scp`'s SFTP protocol
with the destination path untouched. The guest needs OpenSSH Server running
with PowerShell as its default shell — if it is not ready, the wait says
which of SSH, key authentication or the shell is missing, how long it
waited and how to raise the timeout.

### Your own tools

Every profile installs its tools with the scripts it lists in
`provision.scripts` and then proves them with the commands it lists in
`tools.verify`. The shipped baselines are:

| Profile | Tools | Install source |
|---|---|---|
| `macos-*` | Git, Python, uv, curl, wget, jq, vim, tree (+ Ghidra, LLVM for `-re`) | Homebrew |
| `kali-*` | Python 3 + pip/venv, Git, curl, unzip, file, GDB, binutils, build-essential | `apt-get`, from Kali's own repositories |
| `windows-*` | OpenSSH Server, Python 3, Git, 7-Zip (PowerShell ships with Windows) | `winget`, plus the OpenSSH Windows capability |

Each script names the install source and the verification command for every
tool it installs, logs each step with its exit code, and is idempotent, so
re-running `profile build` against an already provisioned image is a no-op
for anything already there. A failed install fails the build; nothing is
skipped silently.

To add your own:

```sh
$ cp "$WARGAME_HOME/profiles/scripts/custom/my-tools.sh" \
     "$WARGAME_HOME/profiles/scripts/kali-custom/my-tools.sh"
# edit it, then add it to the profile after the baseline script:
#   provision:
#     scripts:
#       - scripts/kali-custom/common.sh
#       - scripts/kali-custom/my-tools.sh
#   tools:
#     verify:
#       - ropper --version
$ wargame profile build kali-custom      # re-provisions and publishes a new image
```

`profiles/scripts/custom/my-tools.sh` (and `my-tools.ps1` for Windows) is a
commented starting point: it is not run by any profile, and it spells out
the three rules a provisioning script has to keep — idempotent, never
interactive, non-zero exit fails the build (`exit 3010` on Windows means
"reboot me first, then carry on").

Everything under `$WARGAME_HOME/profiles/` is yours. A new binary re-seeds
the profiles and scripts it ships with, but only over a copy that still
matches what the previous binary wrote: a file you have edited — including
one of the built-in scripts — is never overwritten. `profile init` goes
further and gives a copied profile its own `scripts/<name>/` directory, so
its scripts share no path with the templates at all.

```sh
$ wargame search out_of_bound
$ wargame open dreamhack:11
$ wargame launch dreamhack:11 --profile macos-re-arm64 --network isolated
Launching dreamhack:11 into profile macos-re-arm64 (isolated network)…
… downloading out_of_bound.zip
✓ downloaded out_of_bound.zip (1.2 KiB, sha256 03cb4114beed…)
… booting the VM (up to a few minutes on first boot)
✓ guest ip: 192.168.…
… copying files into the guest and unpacking them there

Ready in 21s: dh-out-of-bound-a1b2c3
Next:
  wargame ssh dh-out-of-bound-a1b2c3      # open a shell in the VM
  wargame gui dh-out-of-bound-a1b2c3      # open a graphical console
  wargame stop dh-out-of-bound-a1b2c3     # pause it (resume later)
  wargame delete dh-out-of-bound-a1b2c3   # discard it

$ wargame ssh            # no id needed: the most recent session
```

Session ids never have to be typed in full: any unique prefix or substring
of the id *or the challenge title* works (`wargame ssh out_of`, `wargame
stop a1b2`), and omitting the id entirely means the most recent session.
An ambiguous reference lists the candidates instead of guessing.

`launch` clones the built profile image, boots it, streams the challenge's
files to a local staging directory (hashing them as they arrive), copies
them into the guest as opaque bytes, and runs the guest-side bootstrap
script — which lays out `incoming/ work/ notes/ output/` and, unless
`--no-extract` is passed, unpacks known archive types (zip/tar) *inside the
guest*. Nothing is ever extracted, previewed, or executed on the host.

When you're done:

```sh
$ wargame sessions                            # alias: wargame ls
┌────────────────────────┬──────────────┬─────────┬─────────┬──────────┬────────────────┬──────┬─────┐
│ id                     ┆ title        ┆ state   ┆ backend ┆ network  ┆ profile        ┆ keep ┆ age │
╞════════════════════════╪══════════════╪═════════╪═════════╪══════════╪════════════════╪══════╪═════╡
│ dh-out-of-bound-a1b2c3 ┆ out_of_bound ┆ running ┆ tart    ┆ isolated ┆ macos-re-arm64 ┆      ┆ 7m  │
│ dh-rev-basic-9f0e1d    ┆ rev-basic-0  ┆ stopped ┆ utm     ┆ isolated ┆ kali-custom    ┆ yes  ┆ 2h  │
└────────────────────────┴──────────────┴─────────┴─────────┴──────────┴────────────────┴──────┴─────┘
2 session(s), 1 up. Ids may be shortened to any unique prefix.

$ wargame delete out_of          # discard the clone (alias: rm)
$ wargame keep out_of            # or: mark it to survive `prune` (delete then needs --force)
$ wargame stop out_of            # or just stop it for now
$ wargame resume out_of          # ...and pick it back up later
$ wargame session inspect out_of # everything recorded about it, incl. the run.log path
```

Every command ends with a `Next:` line naming the natural follow-up, and
`wargame --help` carries a quick-start block. Shell completion is one
command away:

```sh
wargame completions zsh > ~/.zfunc/_wargame      # or: bash, fish, elvish, powershell
```

Running `wargame` with no subcommand opens the TUI, which drives the same
application services (search → problem detail → launch → sessions) as the
CLI above — there is no TUI-only business logic. Inside it, the footer
always lists exactly the keys that do something on the current screen;
`Ctrl+C` quits from anywhere, `[`/`]` page through search results, and on
the sessions screen `Enter` opens a shell, `g` a window, `x`/`r` stop and
resume, `k` toggles keep, `d` deletes (after a `y/N` prompt), `R` refreshes.

## What "isolated" networking means

`wargame launch` defaults to `--network isolated`. Concretely, that's
`tart run --net-host`: the guest is given Tart's host-only vmnet interface,
which can reach the Mac's own network stack (so SSH from the host still
works) but cannot reach the LAN or the internet. This is the right default
for offline reversing/file-analysis challenges — plausible malware inside a
challenge archive has no network to call out on.

**This requires root, every time.** Tart implements `--net-host` (and
`--net-softnet`) by spawning its own helper process, Softnet, and Softnet
needs root to set up the host-only interface. Tart launches it with
non-interactive `sudo`, so unless passwordless sudo is set up for it,
Softnet exits immediately (`root privileges are required to run and
passwordless sudo was not available`) and the VM never gets an IP —
without the check below, `wargame launch --network isolated` would just
spin until the boot timeout with no useful error. Set up passwordless sudo
for Softnet once, replacing the path with wherever `which softnet` (or
`brew --prefix`) puts it on your machine:

```sh
echo "$(whoami) ALL=(root) NOPASSWD: /opt/homebrew/bin/softnet" | sudo tee /etc/sudoers.d/wargame-softnet
```

Until that's in place, `wargame doctor` reports the `network:isolated`
check as failing (with this exact remediation), and `wargame launch
--network isolated` refuses to run — it checks during preflight, *before*
creating a session or a VM, and fails fast with `NetworkUnavailable`
instead of timing out 300 seconds later with a cryptic guest-boot error.

`--network nat` gives the guest Tart's normal shared networking (outbound
internet access) for challenges that need it — remote challenge servers,
package installs, web problems — and needs no sudo rule, so it's the
alternative if you don't want to set one up. Because it also materially
changes the threat model, `wargame` always shows an explicit warning and
asks `Continue? [y/N]` before launching with NAT, unless you pass `--yes`:

```
Warning: NAT networking connects this VM directly to your network, rather
than isolating it (MVP.md §18).
Continue? [y/N]
```

`wargame profile build` always uses NAT regardless of your default —
provisioning needs Homebrew and package downloads, so it wouldn't work
isolated even if you had the Softnet rule configured.

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
  or `wargame prune` for the VMs a failed or interrupted session left on
  disk). `prune` decides what it owns from this home's own records, never
  from a VM's name: a VM another `WARGAME_HOME` or you made is reported and
  left alone, and a running VM is never deleted.
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
  → "wargame profile build <profile>"), so failure modes are
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
- **`network:isolated` fails / `NetworkUnavailable` on `wargame launch
  --network isolated`** — Softnet (which `--net-host` needs) isn't allowed
  to run under passwordless sudo yet; see "What \"isolated\" networking
  means" above for the exact `sudoers.d` line, or pass `--network nat`
  instead.
- **Guest boot times out, or fails immediately with "VM exited during
  boot"** — check `$WARGAME_HOME/logs/wargame.log` and the session's
  `run.log` under `$WARGAME_HOME/sessions/<id>/`. A quick exit whose log
  mentions Softnet is the same root-privilege issue as the previous bullet
  (`wargame doctor` and `launch --network isolated` now catch this before
  creating anything, but an older `run.log` from before that check can
  still show it); an actual multi-minute hang is more likely a cold Tart
  image taking longer than the configured `boot_timeout_secs` (`[vm.tart]`
  in `config.toml`) to get a DHCP lease.
- **`provider:dreamhack` unreachable / `AuthenticationRequired`** — check
  network access to `https://dreamhack.io`, then `wargame auth login`
  again; `wargame auth status` shows the current state without touching
  anything.
- **`session … is ambiguous`** — the shortened id you typed matches more
  than one session; the error lists them. Use a longer prefix or the full
  id from `wargame sessions`.
- **`session is stopped and cannot become running`** (on `ssh`/`gui`) — the
  VM is stopped, possibly because its Tart window was closed; `wargame
  resume <id>` first. `wargame stop` on an already-stopped session (and
  `resume` on a running one) is not an error — it just tells you so.
- **`sshpass` missing** — only needed for `wargame profile build` (the
  one-time password-based key install); everyday `launch`/`ssh` never uses
  it.
- **Config file** lives at `$WARGAME_HOME/config/config.toml` (all fields
  optional; see `DESIGN.md` §3.5 for the schema) — `$WARGAME_HOME` defaults
  to `~/Library/Application Support/wargame-vm` and can be overridden with
  the `WARGAME_HOME` env var or `--home` (the non-e2e test suite does this,
  with a temporary directory, since it never boots a real VM;
  `scripts/e2e-smoke.sh` deliberately does *not* — see below).

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

Four more layers exist for real hardware, documented rather than run in CI.
None of them runs under a plain `cargo test`, `cargo test -- --ignored` or
`nix flake check`: each needs an environment variable set on purpose, because
each boots a real VM (spec.md §9, "실VM 검사는 opt-in이다").

**The first OS installation is not in any of them.** Making a Kali or Windows
guest from an ISO is a manual step — `wargame profile create <p> --iso <path>`,
install the OS through `wargame profile console <p>`, `wargame profile eject
<p>`, then `wargame profile build <p>` — and its evidence (what was clicked,
what the guest ended up with) is recorded separately in
`docs/utm-compatibility.md` and the task reports under `.superpowers/sdd/PLAN/`.
The scripts below start where that leaves off: a profile whose image is already
published.

- **`scripts/e2e-smoke.sh [profile-name]`** — drives the *real* `wargame`
  binary against a *real* Tart VM (still with `WARGAME_PROVIDER=fixture`, so
  no Dreamhack account needed): builds, runs `doctor`, launches the fixture
  challenge into the named profile (default `macos-minimal-arm64`, which
  must already be built), verifies the guest-extracted file over SSH, then
  deletes the session. It uses your **real** `$WARGAME_HOME` (not a
  temporary one) because the profile image only trusts the guest SSH key
  that already lives there from `wargame profile build`, and it never
  deletes that directory. It launches with `--network nat` by default (set
  `WARGAME_SMOKE_NETWORK=isolated` to exercise isolated mode instead, once
  you've set up the Softnet sudoers rule from "What \"isolated\" networking
  means" above). Run it from inside `nix develop`, with the named profile
  already built once (`wargame profile build macos-minimal-arm64`).
- **`scripts/e2e-multi-os.sh <profile> [challenge]`** — the same idea for
  *any* backend and any guest OS, and the one to use for the UTM profiles:
  it takes a profile whose image is already built and verifies everything
  after `launch`. It fails fast, before creating anything, if `wargame
  profiles` does not show that profile's image as `ready`, and points at
  `profile create`/`profile build`. What it checks: the launch itself, the
  session reaching `ready`, that the session recorded the backend, mode,
  guest and network it was asked for, `uname -m` in the guest matching the
  profile's architecture (which is what proves an `emulate` x86_64 guest
  really is one), the `incoming/work/notes/output` workspace tree under the
  profile's workspace root, every staged file's SHA-256 recomputed *in the
  guest* against the host's staging hash, then `stop` → `resume` → SSH again
  → `delete`, and finally that the profile's installation VM and image VM are
  still listed, unchanged. It prints a per-stage timing table. Any check it
  cannot perform prints `UNVERIFIED: <why>` and is listed again in the final
  summary, so "passed" and "passed without checking the guest" never read the
  same.

  It defaults to `WARGAME_PROVIDER=fixture` (no Dreamhack account, no
  download); set `WARGAME_PROVIDER=dreamhack` after `wargame auth login` and
  pass a `dreamhack:<id>` ref to run the same checks against a real
  challenge. Other knobs: `WARGAME_E2E_NETWORK` (default `nat`),
  `WARGAME_E2E_SSH_WAIT` (default 300 s), `WARGAME_BIN` to drive an
  already-built binary. Like `e2e-smoke.sh` it uses your **real**
  `$WARGAME_HOME` and never deletes it — export `WARGAME_HOME` when the
  profiles and build records you mean live somewhere else. Its cleanup trap
  deletes only the sessions this run created; the installation VM and the
  image VM are never stopped, modified or deleted.

  ```sh
  scripts/e2e-multi-os.sh kali-arm64             # Kali arm64, virtualize
  scripts/e2e-multi-os.sh kali-x86_64            # Kali x86_64, emulate
  scripts/e2e-multi-os.sh macos-re-arm64         # the existing Tart path
  ```

  The guest-side checks are POSIX-only. Against a Windows profile the
  lifecycle stages still run and the guest checks report `UNVERIFIED` naming
  `guest/bootstrap.ps1`, rather than pretending to have looked.
- **`cargo test -p wargame-utm --test e2e -- --ignored`** — the UTM adapter
  on its own, against a real UTM and a real published image: the recorded VMs
  are listed and stopped, a clone of the published image starts, gets an
  address from the QEMU guest agent, stops (through the T12 orphan check) and
  is deleted, the installation VM and image VM survive unchanged, and a UUID
  this app never created is `Missing` rather than something to delete. No
  guest login is involved. It needs **both** `WARGAME_E2E=1` and
  `WARGAME_E2E_UTM_PROFILE=<profile>`, and it skips — with a printed reason,
  not a failure — when either is missing, or on a host with no UTM, which is
  why it also compiles and passes on Linux:

  ```sh
  WARGAME_E2E=1 WARGAME_E2E_UTM_PROFILE=kali-arm64 \
    cargo test -p wargame-utm --test e2e -- --ignored --nocapture
  ```

  The only VM it creates is `wg-build-e2e-<pid>`, and it deletes it however
  the test ends.
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
  wargame-tart/        Tart VM backend (implements VmProvider) + the Tart
                       half of the profile image build
  wargame-utm/         UTM/QEMU VM backend (Kali, Windows) + the UTM half of
                       the profile image build
  wargame-cli/         CLI (clap) + TUI (ratatui) composition root
profiles/              built-in profile YAMLs + their provisioning scripts
guest/                 bootstrap.sh / bootstrap.ps1, run inside the guest only
scripts/               manual real-VM verification (see Testing):
  e2e-smoke.sh          macOS/Tart smoke test
  e2e-multi-os.sh       any profile, launch onwards (Kali, Windows, macOS)
```

`wargame-dreamhack`, `wargame-tart` and `wargame-utm` each depend only on
`wargame-core` — never on each other — so a challenge-source change can
never leak into a VM backend, and the two backends cannot leak into each
other.

See `MVP.md` for the product spec and `DESIGN.md` for the full technical
design this implements.
