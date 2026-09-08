# DESIGN.md — wargame-vm technical design

> Refines `MVP.md` into concrete, buildable decisions.
> Scope: the first MVP (M0–M4). Every "future" item in the PRD stays an
> interface seam, not an implementation.
> Date: 2026-09-05

---

## 0. Decisions at a glance

| Area | Decision | Why |
|---|---|---|
| Language | Rust 2021, single Cargo workspace, 4 crates | PRD §34 leans Rust; crate boundaries enforce NFR-05 at compile time |
| Build / reproducibility | Nix flake (`nixpkgs` unstable, `rustPlatform.buildRustPackage`, pinned `Cargo.lock`) | `nix build`, `nix run`, `nix develop`, `nix flake check` are the only entry points anyone needs |
| VM backend | Tart CLI (`/opt/homebrew/bin/tart` or `pkgs.tart`) driven via argv arrays | PRD §12; no native Virtualization.framework binding |
| Guest transport | System `ssh`/`scp` with a per-install ed25519 key; `sshpass` only during image build | PRD Q3 recommendation; keys stay host-side, guest never sees host keys |
| Wargame provider | Dreamhack JSON API (`/api/v1/wargame/...`), Django session-cookie auth | Verified live on 2026-09-05; no HTML scraping |
| Credential store | macOS Keychain via `/usr/bin/security -i` (stdin, never argv) | PRD §9; no plaintext, no argv leak |
| Isolated network | `tart run --net-host` (host-only vmnet via Softnet; needs a sudoers rule for `softnet`, detected by `doctor` and launch preflight) | Guest can reach only the host bridge; SSH still works. Without the rule, launch refuses isolated mode before creating anything |
| NAT network | Tart default shared networking | Explicit opt-in with confirmation |
| GUI console | Stop headless run, re-run with Tart window | PRD Q2: delegate; VNC descriptor is a future console type |
| Archive extraction | Guest-side only, opt-in per launch (`--extract`, default on), never executes | PRD Q4 |
| TUI | ratatui + crossterm, thin layer over the same application services as the CLI | PRD §17: no TUI-only business logic |
| Storage | `$WARGAME_HOME` (default `~/Library/Application Support/wargame-vm`) | Overridable so tests and CI are hermetic |

---

## 1. Verified external facts (probed 2026-09-05)

### Host
- macOS 26.6.2, Apple Silicon (arm64), Tart 2.32.1, Softnet installed, Determinate Nix 3.21.9.
- Existing local Tart images: `ghcr.io/cirruslabs/macos-sequoia-vanilla:latest` (usable as the MVP base image).
- `nixpkgs#tart` = 2.30.6, `nixpkgs#sshpass` = 1.10, `nixpkgs#rustc` = 1.97.1.

### Tart CLI surface used
| Operation | Command |
|---|---|
| list | `tart list --format json` → `[{Name, Source, State, Running, Size, Disk, Accessed}]` |
| clone | `tart clone <src> <dst>` |
| resources | `tart set <vm> --cpu N --memory MiB --disk-size GiB` |
| run headless | `tart run <vm> --no-graphics --no-clipboard --no-audio [--net-host]` (blocks while VM runs) |
| run with window | `tart run <vm> --no-clipboard --no-audio [--net-host]` |
| ip | `tart ip <vm>` (fails until DHCP lease exists) |
| stop / delete | `tart stop <vm>`, `tart delete <vm>` |
| pull | `tart pull <oci-ref>` |

`--net-host` restricts the guest to the host-only network, but Tart implements it (like `--net-softnet`) by spawning Softnet with non-interactive sudo. Verified 2026-09-05: without a sudoers rule the runner exits at once with `root privileges are required to run and passwordless sudo was not available`. The backend therefore probes `sudo -n <softnet> --help` in `healthcheck()` and reports `isolated_network`; launch preflight rejects isolated mode with the remediation hint when it is unavailable. `tart set --disk-size` must be strictly larger than the current disk, so it is passed only when the profile grows the clone. Guest agent (`tart exec`) is absent from vanilla images, so SSH is the transport; `ssh host cmd` runs a non-login zsh, so provisioning also writes the Homebrew PATH into `~/.zshenv`.

### Dreamhack API surface used
| Purpose | Request | Auth | Verified |
|---|---|---|---|
| Search | `GET /api/v1/wargame/challenges/?search=<q>&page=<n>&page_size=<k>[&category=<c>]` → `{count, next, previous, results[]}` | none | yes |
| Detail | `GET /api/v1/wargame/challenges/<id>/` → `{id, title, description(markdown), tags[], tier_display, needs_vm, repository, ...}` | none | yes |
| Login | `POST /api/v1/auth/login/` body `{"email","password"}`; `WWW-Authenticate: Session`; 401 on failure | none | yes (failure path) |
| Download | `POST /api/v1/wargame/challenges/<id>/download/` (no body) → 200 JSON body that is a **bare string** holding a presigned DigitalOcean Spaces URL (`sfo2.digitaloceanspaces.com/wargame1/public/<uuid>.zip?X-Amz-…`); the object answers with `Content-Disposition: attachment; filename="<uuid>.zip"`. 401 without a session; 403 `{"detail":"CSRF Failed: …"}` without the token | `sessionid` cookie + `X-CSRFToken` (value of the **`csrf_token`** cookie, not Django's default `csrftoken`) + `Referer` | yes, live on 2026-09-06 (rev-basic-1) |

The adapter accepts these shapes and fails with `ProviderChanged` otherwise:
1. JSON body that is a bare string URL (the live Dreamhack shape) → follow with GET, raw bytes. Filename: server `Content-Disposition`, unless its stem is a bare UUID, in which case the asset placeholder `<repository>.zip` is used when the extension matches; else the URL's last path segment.
2. JSON object containing a URL field (`url`, `file`, `download_url`, or `files[]{url|file, name|filename}`) → same, defensive fallback.
3. Binary body with `Content-Disposition: attachment; filename=...` → raw bytes directly.

Public search/detail responses are stored as sanitized fixtures under `crates/wargame-dreamhack/fixtures/`.

---

## 2. Repository layout

```
.
├── flake.nix / flake.lock         # nix build, run, develop, check
├── Cargo.toml                     # workspace
├── rust-toolchain.toml            # informational; nix provides the toolchain
├── crates/
│   ├── wargame-core/              # domain, application services, ports, security, infra
│   ├── wargame-dreamhack/         # WargameProvider impl (depends on core only)
│   ├── wargame-tart/              # VmProvider impl (depends on core only)
│   └── wargame-cli/               # binary `wargame`: clap CLI + ratatui TUI (depends on all)
├── profiles/
│   ├── macos-minimal-arm64.yaml
│   ├── macos-re-arm64.yaml
│   └── scripts/macos/{common.sh,reversing.sh}
├── guest/
│   └── bootstrap.sh               # trusted guest bootstrap (embedded via include_str!)
├── docs/
│   └── (generated plans / notes)
├── DESIGN.md, PLAN.md, MVP.md, README.md
└── .claude/workflows/             # multi-agent build workflow (see PLAN.md)
```

Dependency rule (NFR-05), enforced by Cargo:
`wargame-dreamhack` and `wargame-tart` depend only on `wargame-core`; neither depends on the other. Only `wargame-cli` wires them together.

---

## 3. `wargame-core`

### 3.1 Modules

```
core/
  domain/
    challenge.rs      Challenge, ChallengeSummary, ChallengeAsset, ChallengeRef ("dreamhack:11")
    profile.rs        Profile (YAML schema below), GuestOs, GuestArch, Resources
    session.rs        Session, SessionState (+ allowed transitions), NetworkMode, KeepPolicy
    compat.rs         Launchable(profile, capabilities) -> Result<(), Incompatible>
  ports/
    wargame_provider.rs   trait WargameProvider
    vm_provider.rs        trait VmProvider, ProviderCapabilities, VmRef, VmStatus, ConsoleDescriptor
    credential_store.rs   trait CredentialStore
    session_repo.rs       trait SessionRepository
    guest_transport.rs    trait GuestTransport (exec, copy_in, interactive_shell)
  app/
    search.rs         SearchChallenges
    launch.rs         LaunchChallenge (the pipeline, emits LaunchEvent)
    lifecycle.rs      Stop / Resume / Delete / Keep / Inspect / List / Prune
    console.rs        OpenSsh / OpenGui
    doctor.rs         Doctor (host checks, returns Vec<Check>)
    profiles.rs       ListProfiles / BuildProfile
  security/
    filename.rs       sanitize_filename(server_name) -> SafeName
    staging.rs        StagingDir (0700 dir, 0600 files, drop = delete unless keep_debug)
    redact.rs         Redactor for tracing layer (cookies, Authorization, passwords, key material)
    hash.rs           sha256 streaming writer
  infra/
    process.rs        trait ProcessRunner { run(Cmd) -> Output; spawn_detached(Cmd, log) -> Pid }; RealRunner; FakeRunner (tests)
    fs.rs             AppDirs ($WARGAME_HOME), disk_free_bytes(path)
    ssh.rs            SshGuestTransport (system ssh/scp, key file, StrictHostKeyChecking=no, UserKnownHostsFile=/dev/null, ConnectTimeout)
    keychain.rs       KeychainStore (security -i), FileStore (0600 json; tests / CI), MemoryStore
    json_repo.rs      JsonSessionRepository (sessions/<id>/metadata.json, atomic write via tmp+rename)
  config.rs           Config (TOML) with defaults
  error.rs            WargameError (user-facing categories from PRD §23) + `hint()` text
```

### 3.2 Domain types (Rust sketch)

```rust
pub struct ChallengeRef { pub provider: String, pub id: String }        // "dreamhack:11"
pub struct ChallengeSummary { pub r#ref: ChallengeRef, pub slug: String, pub title: String,
                              pub categories: Vec<String>, pub difficulty: Option<String> }
pub struct Challenge { pub summary: ChallengeSummary, pub description: String, pub tags: Vec<String>,
                       pub assets: Vec<ChallengeAsset>, pub remote_endpoints: Vec<String>, pub source_url: String }
pub struct ChallengeAsset { pub id: String, pub filename: String, pub size: Option<u64>,
                            pub descriptor: serde_json::Value /* provider-opaque */, pub expected_sha256: Option<String> }

pub enum GuestOs { Macos, Linux, Windows }
pub enum GuestArch { Arm64, X86_64 }
pub enum NetworkMode { Isolated, Nat }
pub struct Resources { pub cpu: u32, pub memory_mib: u64, pub disk_gib: u64 }

pub enum SessionState { Creating, Downloading, Booting, Provisioning, Ready, Running, Stopped, Failed, Deleting, Deleted }
pub struct Session { pub id: String, pub display_name: String, pub challenge: ChallengeSummary,
                     pub challenge_snapshot: Challenge, pub vm_provider: String, pub profile: String,
                     pub guest_os: GuestOs, pub guest_arch: GuestArch, pub network: NetworkMode,
                     pub resources: Resources, pub created_at: OffsetDateTime, pub state: SessionState,
                     pub vm_ref: Option<VmRef>, pub assets: Vec<SessionAsset /* filename, sha256, size */>,
                     pub keep: KeepPolicy /* Discard | Keep */, pub last_error: Option<String>,
                     pub runner_pid: Option<u32>, pub guest_ip: Option<IpAddr> }
```

State transitions (anything else is a bug and returns `InvalidTransition`):

```
Creating → Downloading → Booting → Provisioning → Ready → Running ⇄ Stopped
any non-terminal → Failed
Ready|Running|Stopped|Failed → Deleting → Deleted
Stopped → Booting (resume)
```

`Ready` means bootstrap finished; the VM is up. `Running` is the same VM after a resume; both count as "up" for `ssh`/`gui`. The record can drift when the VM stops without going through the CLI (the `gui` window is closed, the guest shuts down, the runner dies): every lifecycle read (`sessions`, `inspect`, `stop`, `resume`, `ssh`, `gui`) first reconciles an "up" record against the backend and moves it to `Stopped` when the VM is reported stopped. A `Stopped` session may be `gui`-started directly.

### 3.3 Ports

```rust
#[async_trait] pub trait WargameProvider: Send + Sync {
    fn name(&self) -> &str;                                              // "dreamhack"
    async fn search(&self, q: &SearchQuery) -> Result<SearchPage>;       // SearchPage { items, next_page: Option<u32>, total }
    async fn get_challenge(&self, id: &str) -> Result<Challenge>;
    async fn auth_state(&self) -> Result<AuthState>;                    // Anonymous | Authenticated { display }
    async fn login(&self, username: &str, password: Secret<String>) -> Result<()>;
    async fn download_asset(&self, ch: &Challenge, a: &ChallengeAsset, sink: &mut dyn AssetSink) -> Result<DownloadResult>;
    async fn healthcheck(&self) -> Result<ProviderStatus>;
}
// AssetSink is a StagingFile writer: bytes only, computes sha256, fsyncs, rejects if server filename is unsafe.

#[async_trait] pub trait VmProvider: Send + Sync {
    fn name(&self) -> &str;                                              // "tart"
    fn capabilities(&self) -> ProviderCapabilities;
    async fn healthcheck(&self) -> Result<BackendStatus>;
    async fn image_exists(&self, image: &ImageRef) -> Result<bool>;
    async fn pull_image(&self, image: &ImageRef) -> Result<()>;
    async fn create(&self, spec: &VmSpec) -> Result<VmRef>;              // clone + set resources
    async fn start(&self, vm: &VmRef, opts: &StartOptions) -> Result<RuntimeInfo>; // detached; returns pid
    async fn wait_ip(&self, vm: &VmRef, timeout: Duration) -> Result<IpAddr>;
    async fn stop(&self, vm: &VmRef) -> Result<()>;
    async fn delete(&self, vm: &VmRef) -> Result<()>;
    async fn status(&self, vm: &VmRef) -> Result<VmStatus>;             // Running | Stopped | Missing
    async fn list(&self) -> Result<Vec<VmRef>>;                          // for orphan detection (prefix wg-)
    fn console(&self, vm: &VmRef) -> ConsoleDescriptor;                  // Window { start_cmd } for Tart
}

#[async_trait] pub trait GuestTransport: Send + Sync {
    async fn wait_ready(&self, ip: IpAddr, timeout: Duration) -> Result<()>;
    async fn exec(&self, ip: IpAddr, argv: &[String]) -> Result<ExecResult>;
    async fn copy_in(&self, ip: IpAddr, src: &Path, dest: &str) -> Result<()>;   // scp, dest is guest path
    fn interactive_shell_cmd(&self, ip: IpAddr) -> Command;                      // for `wargame ssh` (exec replaces process)
}

pub trait CredentialStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Secret<String>>>;
    fn set(&self, key: &str, value: &Secret<String>) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}
```

### 3.4 Launch pipeline (`app/launch.rs`)

Input: `LaunchRequest { challenge: ChallengeRef, profile: String, network: NetworkMode, resources: Option<Resources>, extract: bool, gui: bool }`.
Output: stream of `LaunchEvent` (for CLI progress lines and TUI progress screen) and the final `Session`.

```
1. preflight
   a. load profile; compat::check(profile, vm.capabilities())         → Incompatible
   b. vm.healthcheck()                                                  → BackendUnavailable
   c. image_exists(profile.image_name())                                → ImageMissing (hint: wargame profile build <name>)
   d. disk_free >= profile.resources.disk_gib * 0.25 (clone is COW)    → InsufficientResources
   e. provider.get_challenge(id)                                        → ChallengeNotFound / ProviderUnavailable
   f. if assets non-empty and auth_state == Anonymous                    → AuthenticationRequired
2. session record: id = "dh-<slug>-<6 hex>", state Creating, persisted BEFORE any VM exists (NFR-02)
3. vm.create(spec) → vm_ref persisted immediately
4. state Downloading: for each asset → StagingDir, sha256 recorded in session.assets
5. state Booting: vm.start(headless unless gui) → pid persisted; vm.wait_ip; transport.wait_ready
6. state Provisioning: copy bootstrap.sh + metadata.json + each asset → ~/wargame/<slug>/incoming/ ;
   exec `bash ~/wargame/.wg/bootstrap.sh <slug> --extract=<bool>`
7. staging dir deleted (unless config.debug.keep_staging)
8. state Ready
On any error: state Failed with last_error; VM is left in place so `wargame delete` can clean it; the error message tells the user the session id.
```

Progress events: `Preflight`, `SessionCreated(id)`, `VmCreated`, `Downloading{filename}`, `Downloaded{filename, sha256}`, `Booting`, `GuestIp(ip)`, `Bootstrapping`, `Ready{ssh_hint}`, `Failed{error}`.

### 3.5 Config (`config.rs`)

```toml
[general]
default_provider = "dreamhack"
default_profile  = "macos-re-arm64"
default_network  = "isolated"
extract_archives = true

[vm.tart]
binary = "tart"          # resolved via PATH
vm_prefix = "wg-"
boot_timeout_secs = 300

[wargame.dreamhack]
base_url = "https://dreamhack.io"
credential_store = "keychain"   # keychain | file (file is for tests/CI only and warns)

[debug]
keep_staging = false
```

`$WARGAME_HOME` overrides the root directory. Layout under it: `config/config.toml`, `profiles/`, `sessions/<id>/metadata.json` (+ `run.log`), `keys/id_ed25519{,.pub}`, `cache/`, `logs/wargame.log`.

### 3.6 Security details

- **Filename**: take the last path component, reject empty/`.`/`..`, strip control chars and leading dots, limit to 255 bytes, fall back to `asset-<id>.bin`.
- **Staging**: `tempfile::Builder` under `$WARGAME_HOME/cache/staging/` with mode 0700; files 0600; written via a `.part` name and renamed only after the body ends cleanly and the SHA-256 is computed. Partial downloads never carry the final name.
- **Redaction**: a `tracing` layer that replaces values of fields named `cookie`, `authorization`, `password`, `sessionid`, `csrftoken`, and any string matching `sessionid=…` / `Bearer …`. Unit-tested.
- **Keychain**: service `wargame-vm`, account `dreamhack.session`. Value is a JSON blob `{cookies: [{name,value,domain,path}], created_at}`. Written through `security -i` stdin (`add-generic-password -U -a … -s … -w …` line), read with `find-generic-password -w`. Passwords are never persisted; only the resulting session cookies are.
- **Guest key**: `ssh-keygen -t ed25519 -N ''` into `$WARGAME_HOME/keys/` (0600) on first profile build. It is authorised only inside profile images. It never leaves the host except as the public half.
- **Process invocation**: always `Command::new(bin).args([...])`, never a shell string; every argv is logged (secrets never appear in argv by construction).

---

## 4. `wargame-dreamhack`

```
dreamhack/
  client.rs     reqwest::Client with cookie_store, rustls, UA "wargame-vm/<ver>"; loads cookies from CredentialStore on construction
  api.rs        raw endpoint functions returning serde structs (SearchResponse, DetailResponse, LoginResponse)
  model.rs      serde structs mirroring the API (only fields we need; `#[serde(default)]` liberally)
  normalize.rs  api::* -> core::Challenge / ChallengeSummary   (slug = repository or slugified title; difficulty = tier_display)
  provider.rs   impl WargameProvider
  auth.rs       login flow: GET / (csrftoken cookie) → POST /api/v1/auth/login/ → persist cookie jar to CredentialStore
  download.rs   POST /api/v1/wargame/challenges/<id>/download/ with X-CSRFToken + Referer; handles shapes (1)/(2) from §1
fixtures/       search.json, detail.json, search_page.json (sanitized), login_401.json, download_401.json
```

Asset discovery: the detail response does not list files. `Challenge.assets` is populated with one **logical asset** `{id: "<challenge-id>", filename: "<repository>.zip" (placeholder until the server tells us), descriptor: {"kind":"dreamhack-download","challenge_id":…}}` when `needs_vm == true` or the description mentions files; the real filename is taken from the download response. `download_asset` may therefore return a `DownloadResult` whose filename differs from the placeholder, and the launch pipeline uses the returned name. If the download yields JSON with multiple files, each is written as its own staged asset.

Errors: 401/403 → `AuthenticationRequired`; 404 → `ChallengeNotFound`; unexpected JSON → `ProviderChanged { what }`; network → `ProviderUnavailable`.

---

## 5. `wargame-tart`

```
tart/
  provider.rs   impl VmProvider using ProcessRunner (argv arrays), vm name = "<prefix><session-id>"
  args.rs       pure functions building argv for each op (unit-tested against FakeRunner)
  list.rs       serde for `tart list --format json`
  image.rs      ImageRef { Oci(String) | Local(String) }; profile image local name = "wargame-profile-<name>"
  build.rs      ProfileImageBuilder: clone base → set → start (NAT, headless) → wait ssh (sshpass, guest.user/password)
                → install pubkey → scp profiles/scripts → run each script → run tools.verify → stop
  console.rs    ConsoleDescriptor::Window { argv } ; OpenGui = stop headless → spawn_detached(run with window)
```

Capabilities: `guest_os=[macos], guest_arch=[arm64], graphical_console=true, ssh=true, snapshots=false, copy_in=true, remote=false`.

`start` spawns `tart run …` detached (new session/process group, stdout+stderr → `sessions/<id>/run.log`) and returns the pid. `stop` calls `tart stop`. `status` uses `tart list --format json`.

---

## 6. Profiles

`profiles/*.yaml` (embedded in the binary with `include_str!` and also copied to `$WARGAME_HOME/profiles/` so users can edit/add; a pristine copy under `profiles/.builtin/` lets an unedited YAML follow the binary while an edited one is kept. `profiles/scripts/**` is embedded the same way and rewritten under `$WARGAME_HOME/profiles/scripts/` on every run, since a `nix build` binary has no checkout to read them from):

```yaml
apiVersion: wargame.vm/v1
kind: Profile
metadata: { name: macos-re-arm64, description: "macOS reversing: Ghidra, LLVM, Python/uv" }
guest: { os: macos, arch: arm64, user: admin, password: admin }   # password only used during image build
image: { base: ghcr.io/cirruslabs/macos-sequoia-vanilla:latest }
resources: { cpu: 4, memory_gib: 8, disk_gib: 60 }
provision:
  scripts: [scripts/macos/common.sh, scripts/macos/reversing.sh]
tools:
  verify:
    - python3 --version
    - uv --version
    - test -x /opt/homebrew/bin/ghidraRun
workspace: { root: /Users/admin/wargame }
```

`macos-minimal-arm64` uses only `common.sh` (Homebrew, git, python, uv, curl, wget, vim, jq). `reversing.sh` adds `ghidra` (cask), `llvm`, `binutils`, `hexyl`, `file`.

---

## 7. `wargame-cli`

```
cli/
  cli.rs         clap: search|find, open|show|info, launch|run, sessions|ls|ps, session inspect, ssh, gui, stop,
                 resume|start, delete|rm, keep, prune, doctor, profiles, profile build, auth [login|status|logout],
                 completions <shell>; no subcommand → TUI
  main.rs        dispatch; resolves session references; prints "Next:" hints after every command
  wiring.rs      builds Config, AppDirs, providers, repo, transport (the composition root)
  output.rs      human tables + `--json`
  tui/
    app.rs       state machine: Search → Problem → Launching → Sessions; Esc back, q quit, Ctrl+C quits anywhere;
                 the footer lists only keys that currently do something
    search.rs    input + list, `/` focuses input, Enter opens, `[`/`]` page
    problem.rs   detail (description scrolled), profile picker, network picker (NAT shows warning), Enter/l launches
    launch.rs    event log driven by LaunchEvent, on Ready offers [s] ssh / [g] gui / [b] sessions
    sessions.rs  table with ssh/gui/stop/resume/keep/delete (delete asks y/N), R refreshes
```

Every session-taking command accepts a *reference*, resolved by
`app::lifecycle::resolve_session`: the exact id, else a unique id prefix, else a
unique case-insensitive substring of the id or challenge title; several matches
are an `AmbiguousSession` error listing the candidates. Omitting the reference
(allowed everywhere except `delete`) means the most recently created session.

`wargame ssh <session>` execs `ssh` (replaces the process). `wargame gui` follows §5. `wargame delete` on a `Keep` session requires `--force` or interactive confirmation (NFR-07). `wargame prune` lists Tart VMs with the `wg-` prefix that have no session record and deletes them with confirmation (NFR-02).

`doctor` checks: arm64 + macOS ≥ 14, `tart` on PATH + version, `sshpass` (only needed for build; warning), `ssh`/`scp`/`ssh-keygen`, `security`, disk free, `$WARGAME_HOME` writable, each profile image present, Dreamhack reachability + auth state, guest key present.

---

## 8. Nix

`flake.nix` provides for `aarch64-darwin` (and `x86_64-linux` for cargo-only checks):

- `packages.default` = `rustPlatform.buildRustPackage` (name `wargame`, `cargoLock.lockFile = ./Cargo.lock`, `doCheck = true`).
- `apps.default` = `wargame`.
- `devShells.default` = rustc, cargo, clippy, rustfmt, rust-analyzer, cargo-nextest, sshpass, tart, jq, plus `WARGAME_HOME` unset (user default). `just`-free; `cargo` commands documented in README.
- `checks.{fmt,clippy,tests}`.

Runtime binaries (`tart`, `ssh`, `security`) are resolved from PATH at run time; the package wraps `wargame` with `makeWrapper --prefix PATH : ${tart}/bin` so `nix run` works without Homebrew.

---

## 9. Testing

| Layer | How | Runs where |
|---|---|---|
| core unit | filename sanitization, staging rename semantics, redaction, state transitions, compat matrix, config defaults, launch pipeline with fake provider/vm/transport (`FakeRunner`) | `cargo test`, `nix flake check` |
| dreamhack fixtures | parse `fixtures/*.json`, normalization, 401 → AuthenticationRequired, unexpected shape → ProviderChanged, CSRF header construction, redaction of `Set-Cookie` | same |
| tart argv | every operation's argv against `FakeRunner`; `tart list` JSON parsing | same |
| cli | `wargame --help`, `doctor --json` on a fake environment, `sessions --json` on a temp `WARGAME_HOME` | same |
| VM integration | `cargo test -p wargame-tart --features e2e -- --ignored` : clone vanilla → run → ssh → copy fixture zip → bootstrap → verify → delete | manual on the Mac; documented |
| E2E smoke | `scripts/e2e-smoke.sh`: uses a local fixture challenge (`FixtureProvider` behind `WARGAME_PROVIDER=fixture`) so it needs no Dreamhack account | manual |

---

## 10. Out of scope confirmations

Not built in MVP (kept as seams): Hyper-V remote agent, Linux guests, Softnet policy, OCI registry for profile images, VNC console, browser login, multiple providers. `ConsoleDescriptor`, `ProviderCapabilities`, and `ImageRef::Oci` exist so those can be added without touching `app/`.
