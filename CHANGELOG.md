# Changelog

All notable changes to `wargame-vm` are recorded here.

Entries for the next release are kept as [sacho](https://sacho.dev/) fragments
under `changes.d/` — one file per user-visible change — and compiled into the
generated region below. Never edit that region, or a released section, by
hand; add or edit a fragment instead.

This project has not made a versioned release yet. The `[Unreleased]`
sections after the generated region are frozen notes from the milestones that
predate sacho, kept for history.

<!-- sacho:unreleased:begin -->


Unreleased
----------

To be released.

 -  Challenges can now be solved on Kali Linux and Windows guests, not only
    macOS. Three profiles ship with the binary: `kali-arm64`, `kali-x86_64`
    and `windows-x86_64`.

 -  A profile now says which processor architecture its guest uses. An arm64
    guest runs virtualized at close to native speed; an `x86_64` guest runs
    emulated, so expect it to be markedly slower and to need far longer to
    boot the first time (30 minutes is the default patience, raise it with
    `[vm.utm].boot_timeout_secs`).

 -  The Kali and Windows profiles have not yet been driven end to end
    against a fully installed guest. Treat them as ready to try, not as
    proven.

 -  Guests that Tart cannot host are now run through UTM. You never choose
    the virtualizer yourself: each profile names the guest it wants and
    `wargame` picks the one that can run it, so macOS guests keep working
    exactly as before.

 -  `wargame profiles` shows what that resolved to — the guest system and
    architecture, the virtualizer and whether it virtualizes or emulates,
    and the profile's image state — and `wargame doctor` reports the health
    of every virtualizer you actually have profiles for. A session made with
    a profile whose virtualizer this build cannot drive still lists and
    still deletes; only starting it is refused.

 -  The same four answers appear wherever you pick or watch a profile: the
    interactive profile picker shows them next to each name, `wargame launch`
    prints what the profile resolved to and which image it will clone before
    it creates anything, and `wargame sessions` and `wargame session inspect`
    name the virtualizer each session actually runs on. That last one matters
    once macOS and Kali sessions sit in the same list — every stop, resume,
    delete or console goes to the session's own virtualizer, including for
    sessions created before wargame had more than one.

 -  Kali and Windows have no ready-made base image to clone, so `wargame`
    now walks you through installing one yourself, once:

     -  `wargame profile init <template> --name <yours>` copies a bundled
        profile under your own name, with its own provisioning scripts that
        a later upgrade will not overwrite.
     -  `wargame profile create <yours> --iso <path>` builds a long-lived
        installation VM from an installer image, checking `--sha256` against
        it first if you pass one. `--from-vm <uuid>` starts from a VM you
        already installed instead; that VM is only ever copied, never
        altered. Re-running the command against the same source just
        re-prints what to prepare in the guest; pointing it at a different
        source is refused rather than quietly making a second VM.
     -  `wargame profile console <yours>` opens the installer window. Install
        the OS, create your user, and leave an SSH server, the guest agent
        and this host's public key in place.
     -  `wargame profile build <yours>` then boots that VM, runs your
        profile's provisioning scripts, checks the tools the profile
        promises, shuts the guest down and publishes the result as the image
        every launch clones.

 -  Until that build succeeds the profile reports that it is still awaiting
    its install, and a build that fails leaves the previous image launchable.
    Neither the installation VM nor a published image is ever removed by
    `wargame prune`, and the installation VM is what the next rebuild starts
    from, so keep it.

 -  Windows 11 additionally wants TPM 2.0 and Secure Boot, which cannot be
    switched on for you; `wargame profile create` prints the one-time step to
    do by hand before you install. You supply the Windows installer and its
    licence.

 -  The Kali and Windows profiles now install a real tool set instead of an
    empty one. Kali gets Python 3 with pip and venv, Git, curl, unzip, file,
    GDB, binutils and build-essential from its own package repositories;
    Windows gets OpenSSH Server, Python 3, Git and 7-Zip through winget, with
    PowerShell left as the shell every wargame command is phrased for. Each
    script says where every tool comes from and how it is checked, logs every
    step with its exit code, and can be re-run against an image it already
    provisioned. A tool that will not install fails the build rather than
    being quietly skipped, and no image is published until every check
    passes.

 -  Adding your own tools is the same flow: drop a script under
    `profiles/scripts/`, list it in the profile's `provision.scripts` after
    the baseline one, add a command per tool to `tools.verify`, and run
    `wargame profile build <profile>` again. A commented starting point ships
    at `profiles/scripts/custom/my-tools.sh` (and `.ps1` for Windows) with
    the rules a provisioning script has to keep.

 -  Anything under `profiles/` that you have edited now survives an upgrade,
    including a change to one of the bundled scripts. Previously only profile
    files were protected and the bundled scripts were rewritten on every run.
    A file you have not touched still follows the tool, so an unedited script
    is never left behind at an old version.

 -  Windows guests are driven through PowerShell. `wargame profile build`
    runs the profile's PowerShell provisioning scripts, and a script that
    asks for a reboot is honoured: the build waits for the guest to come
    back and carries on. `wargame ssh` into a Windows session drops you at a
    PowerShell prompt, and every other command that reaches into the guest
    uses the session's own shell rather than assuming a Unix one.

 -  Commands and paths survive the trip intact — spaces, backslashes,
    quotes, `$`, and non-ASCII text such as Korean all arrive unmangled, and
    the guest command's own exit code comes back to you.

 -  A Windows guest gets the same challenge workspace as every other guest:
    `incoming`, `work`, `notes` and `output`, with the challenge summary
    written alongside them.

 -  `wargame doctor --profile <name>` checks only what that one profile needs
    — its virtualizer, execution mode, network, image and guest tooling —
    instead of everything on the host. Useful when one profile misbehaves and
    the rest are fine.

 -  Challenge files are now checksummed again inside the guest. The host
    records a digest for everything it copies over and the guest recomputes
    each one before anything is unpacked, so a truncated or corrupted
    transfer is caught in the guest rather than silently solved around.

 -  A challenge archive that tries to write outside the place it was unpacked
    is now refused whole, rather than partly extracted. Absolute paths, `..`
    traversal, drive letters, UNC prefixes, reserved Windows device names,
    trailing dots or spaces, control characters and links all disqualify the
    archive.

 -  When that happens the original file stays untouched in `incoming/`, no
    destination directory is created, and the reason is written to
    `notes/archive-refusals.txt` so you can decide what to do with it.
    Archives are still only ever unpacked inside the guest, never on your
    machine.

 -  When a guest never becomes reachable, the error now says what was
    actually missing — the SSH service, key authentication, or the shell
    itself — along with how long it waited, the timeout in force and how to
    raise it. A guest whose SSH service has not started is no longer reported
    as ready.

<!-- sacho:unreleased:end -->

## [Unreleased] — Usability pass (2026-09-08)

### Changed

- **Session references**: every command that takes a session id now also
  accepts a unique prefix or substring of the id or challenge title, and
  (except `delete`) may omit it to mean the most recent session. Ambiguous
  references list their candidates (`WargameError::AmbiguousSession`).
- **Next-step hints**: `launch` ends with an elapsed time and the four
  follow-up commands; `search`, `open`, `sessions`, `session inspect`,
  `stop`, `resume`, `keep`, `profiles`, `profile build` and `auth` each
  end with a `Next:` line. `stop`/`resume` on a session already in that
  state succeed with a message; state errors read "session is stopped and
  cannot become running" and carry a state-specific hint; deleting a kept
  session raises `SessionKept` naming `--force`.
- **Tables**: `sessions` gains `keep`/`age` columns, a summary line and an
  empty-state hint; `search` explains no-result pages; `doctor` renders
  ✓/✗ with a failure summary; `profiles` marks the default.
- **Help**: every flag is documented, defaults are visible, `--network` is
  an enum (case-insensitive), global flags sort last, `wargame --help`
  carries a quick-start block, `profile build` defaults to the configured
  profile, `-y`/`-f` short flags, aliases `find`, `show`/`info`, `run`,
  `ls`/`ps`, `start`, `rm`. New `wargame completions <shell>`.
- **TUI**: Ctrl+C quits anywhere; the footer lists only keys that do
  something on the current screen; "Searching…"/"Opening…" indicators;
  `[`/`]` page through results; Enter launches from the problem screen;
  the launch log is coloured and auto-scrolls; the sessions screen is a
  real table (header, keep, age, state colours, empty state) with `g`
  gui, `k` keep, `R` refresh, and state-aware explanations instead of
  transition errors; async errors show their hint.
- Hints no longer carry a `Run:` prefix; an unwritable `--home` reports
  the path and the fix.

## [Unreleased] — MVP

### Added

- **Core domain & ports** (`wargame-core`): `ChallengeRef`/`Challenge`,
  `Profile`, `Session`/`SessionState` with a validated state machine,
  provider/backend compatibility checks, `WargameError` with per-error
  hints, and the `WargameProvider`/`VmProvider`/`GuestTransport`/
  `CredentialStore`/`SessionRepository` port traits.
- **Security & infra** (`wargame-core`): filename sanitization, streaming
  SHA-256 staging with atomic rename-on-success (partial downloads never
  get a final name), a tracing redaction layer, a `ProcessRunner`
  abstraction (argv arrays only, never shell strings), macOS Keychain and
  plaintext-file credential stores, a JSON session repository, and an SSH
  guest transport — plus a full set of in-memory fakes for testing.
- **Dreamhack provider** (`wargame-dreamhack`): search/detail/download
  against the Dreamhack JSON API with session-cookie auth, CSRF headers,
  and response-shape fixtures.
- **Tart provider** (`wargame-tart`): the Tart CLI driven entirely through
  argv arrays — clone/set/run/ip/stop/delete/list — plus a profile image
  builder that provisions a fresh clone over a temporary password login and
  installs the per-install guest key.
- **Application services** (`wargame-core::app`): `launch` (the full
  search-free-download-boot-provision pipeline), session lifecycle
  (`stop`/`resume`/`delete`/`keep`/`prune`), console access, and `doctor`.
- **Profiles & guest bootstrap**: `macos-minimal-arm64` and
  `macos-re-arm64` profile definitions, their provisioning scripts, and
  `guest/bootstrap.sh` (guest-side-only archive extraction, zip-slip-safe,
  never executes downloaded content).
- **CLI** (`wargame-cli`): the full `wargame` subcommand surface (`search`,
  `open`, `launch`, `sessions`, `session inspect`, `ssh`, `gui`, `stop`,
  `resume`, `keep`, `delete`, `prune`, `doctor`, `profiles`,
  `profile build`, `auth login|status|logout`), a `FixtureProvider` for
  network-free testing, and `--json` output throughout.
- **TUI** (`wargame-cli::tui`): an Elm-style ratatui interface over the
  same application services as the CLI — search → problem detail → launch →
  sessions — with no TUI-only business logic.
- **Docs & tooling** (this change): `README.md` (install, first-run walk-
  through, the isolated-vs-NAT networking model, security summary,
  troubleshooting), `scripts/e2e-smoke.sh` (a real-VM smoke test using the
  fixture provider), `crates/wargame-tart/tests/e2e.rs` (an `#[ignore]`d,
  `WARGAME_E2E=1`-gated M0 proof against a real Tart VM), and this
  changelog.

### Nix

- `flake.nix` provides `packages.default`/`apps.default` (the wrapped
  `wargame` binary, with `tart`/`sshpass`/`openssh` resolved from `PATH` at
  run time), `devShells.default`, and `checks.{build,fmt,clippy,tests}` for
  `aarch64-darwin` and `x86_64-linux`.
