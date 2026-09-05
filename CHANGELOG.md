# Changelog

All notable changes to `wargame-vm` are recorded here. This project has not
yet made a versioned release; entries are grouped by the MVP milestones
defined in `MVP.md` §36 and the implementation plan in `PLAN.md`.

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
