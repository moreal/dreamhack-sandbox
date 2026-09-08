# Changelog

All notable changes to `wargame-vm` are recorded here. This project has not
yet made a versioned release; entries are grouped by the MVP milestones
defined in `MVP.md` §36 and the implementation plan in `PLAN.md`.

## [Unreleased] — Multi-OS guests (2026-09-09)

### Added

- **PowerShell guests**: `wargame-core::infra::powershell` encodes a guest
  command as UTF-16LE + base64 for `powershell -EncodedCommand`, with
  PowerShell literal quoting instead of POSIX quoting, so Windows paths,
  spaces, single/double quotes, `$`, backticks, Korean text and newlines
  reach the guest intact and the command's own exit code (3010 included)
  comes back.
- **`guest/bootstrap.ps1`**: the Windows counterpart of `bootstrap.sh` --
  the same `incoming`/`work`/`notes`/`output` workspace, the same challenge
  metadata summary, the same SHA-256 verification and the same
  archive-safety rules.
- **`wargame-core::infra::GuestOps`**: the single place a guest command's OS
  shape is decided (`mkdir -p` vs `New-Item`, `bash` vs `powershell -File`,
  `sync` vs `Write-VolumeCache`, `/` vs `\`), so no application service
  branches on the guest OS.

### Changed

- **`wargame profile build` provisions Windows** instead of refusing it: the
  profile's `scripts/windows/*.ps1` are copied into the guest and run with
  PowerShell, exit code 3010 waits for the reboot and continues, and
  `tools.verify` runs through the same OS-aware exec. A build whose guest
  shell has no wired transport is still refused before the VM is touched.
- **`wargame ssh` into a Windows session lands in PowerShell**, and every
  lifecycle step (bootstrap, flush-before-stop, resume, gui) uses the
  session's saved shell rather than assuming POSIX.
- **Guest readiness** distinguishes SSH, key authentication and the shell
  itself, and reports which is missing, how long it waited, the timeout used
  and how to raise it (`WargameError::GuestNotReady`); a missing service is
  never reported as ready.
- **Transferred files are verified in the guest**: the host stages an
  `incoming/.wargame-sha256` list and both bootstrap scripts recompute every
  digest before anything is unpacked.
- **Archive extraction refuses a whole archive** when any entry is absolute,
  traverses with `..` or `..\`, carries a drive letter, UNC prefix or `:`,
  is a reserved Windows device name, ends in a dot or space, contains a
  control character, or is a symbolic/hard link. The original stays in
  `incoming/`, no destination directory is created, and the reason is
  reported in `notes/archive-refusals.txt`. Extraction still happens only in
  the guest.

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
