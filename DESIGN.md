# DESIGN.md — wargame-vm technical design

> Refines `MVP.md` into concrete, buildable decisions, and records the
> multi-OS work specified by `.superpowers/sdd/PLAN/spec.md`.
> Scope: the MVP (M0–M4) plus the Tart + UTM multi-backend feature.
> Dates: MVP 2026-09-05; multi-OS revision 2026-09-09
> (branch `feature/multi-os-vm`, HEAD `44f8d05`).
>
> **Numbering.** §1–§7 and §9 keep the meanings the MVP gave them, because
> source comments cite them by number. §8 (was "Nix") and §10 (was "Out of
> scope") carried no citations, so the new material took those numbers and
> the displaced sections moved to §11 and §13.
>
> **Discipline.** This document records what was *verified*, not what was
> hoped. External behaviour is stated only where a command produced the
> quoted result; everything else is marked unverified and §10 says what is
> still owed.

---

## 0. Decisions at a glance

| Area | Decision | Why |
|---|---|---|
| Language | Rust 2021, single Cargo workspace, **5 crates** | PRD §34 leans Rust; crate boundaries enforce NFR-05 at compile time |
| Build / reproducibility | Nix flake (`nixpkgs` unstable, `rustPlatform.buildRustPackage`, pinned `Cargo.lock`) | `nix build`, `nix run`, `nix develop`, `nix flake check` are the only entry points anyone needs |
| VM backends | **Two**: Tart CLI for macOS/arm64, UTM 4.7.5 (`utmctl` + AppleScript) for Linux/Windows on arm64 and x86_64. Both sit behind the same `VmProvider` port | spec.md §5: keep the working macOS path, add UTM rather than a second bespoke QEMU driver |
| Backend selection | Pure-data resolver over a static support matrix. A **new launch** resolves from the profile; **resume/stop/delete/ssh/gui** route from the session's own snapshot. No runtime fallback to another backend | spec.md §3 FR-1/FR-5 |
| Execution mode | `virtualize` \| `emulate`, declared per profile and never substituted silently | spec.md §1: an unavailable `virtualize` is an error, not a downgrade |
| Guest transport | System `ssh`/`scp` for **every** guest; Windows commands cross the wire as `powershell -EncodedCommand` (UTF-16LE + base64). `sshpass` only during a Tart image build | spec.md §3 FR-4; POSIX quoting is never reused for Windows |
| Guest OS shape | One seam, `infra::GuestOps`: application services ask for "make these directories"/"run this script" and never test the guest OS themselves | spec.md §5 item 3 |
| Wargame provider | Dreamhack JSON API (`/api/v1/wargame/...`), Django session-cookie auth | Verified live 2026-09-05/06; no HTML scraping |
| Credential store | macOS Keychain via `/usr/bin/security -i` (stdin, never argv) | PRD §9 |
| Isolated network | Tart: `tart run --net-host` (host-only vmnet via Softnet; needs a sudoers rule). UTM: network mode `host` (`-netdev vmnet-host`, no sudo, no helper) | Guest-side isolation is **verified for Tart**, **unverified for UTM** (§10) |
| NAT network | Tart's default shared networking; UTM's `shared`. Image builds always run on NAT | Builds install packages; sessions default to isolated |
| GUI console | Tart: stop the headless run, re-run with a window. UTM: `utmctl start <uuid>` (there is no working `attach`, and `--hide` is broken) | docs/utm-compatibility.md §3, T7 |
| Reusable images | One build flow in core (`app::profile_build`), backend steps behind the `ProfileImageBuilder` port; one `BuildRecord` per profile under `$WARGAME_HOME/builds/` | spec.md §5 items 4–5 |
| Archive extraction | Guest-side only, opt-in per launch (`--extract`, default on), never executed, entry names checked before anything is written | PRD Q4, spec.md §3 FR-4 |
| TUI | ratatui + crossterm, thin layer over the same application services as the CLI | PRD §17: no TUI-only business logic |
| Storage | `$WARGAME_HOME` (default `~/Library/Application Support/wargame-vm`), now with `builds/` beside `sessions/` | Overridable so tests and CI are hermetic |
| Changelog | [sacho](https://sacho.dev/) fragments under `changes.d/`, one per user-visible change | `CHANGELOG.md` is generated; see §12 |

---

## 1. Verified external facts

### Host (probed 2026-09-05, extended 2026-09-09)
- macOS 26.6.2 (build 25G83), Apple Silicon (arm64), 10 CPUs, 64 GiB RAM.
- Tart 2.32.1 (Homebrew) / 2.36.0 (nixpkgs wrapper), Softnet installed, Determinate Nix 3.21.9.
- **UTM 4.7.5** (`CFBundleVersion` 118, bundle id `com.utmapp.UTM`), installed with `brew install --cask utm`; `utmctl` at `/Applications/UTM.app/Contents/MacOS/utmctl`, symlinked to `/opt/homebrew/bin/utmctl`.
- Existing local Tart image: `ghcr.io/cirruslabs/macos-sequoia-vanilla:latest` (the MVP base image).
- `nixpkgs#tart` = 2.30.6, `nixpkgs#sshpass` = 1.10, `nixpkgs#rustc` = 1.97.1. Nix does **not** provide UTM.app: it is a macOS application runtime dependency, not a build dependency (§11).

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

### UTM surface used

The full record — every command, its raw output, and twelve numbered traps
(T1–T12) — is **[`docs/utm-compatibility.md`](docs/utm-compatibility.md)**.
It is the evidence; this document only records the decisions taken from it.
The load-bearing findings, with the section that proves each:

| Finding | Consequence for this design | Evidence |
|---|---|---|
| `utmctl`'s exit code does not report Apple-Event failures (T1) | A call fails when stderr carries `Error from event:`/`Error:`, whatever the exit code | §4 T1 |
| `duplicate` returns nothing (T2); display names are not unique and `virtual machine named` silently picks one (T3) | Every operation is addressed by **UUID**; a clone's UUID is recovered by a registry set difference | §4 T2/T3 |
| The configuration record is not round-trippable (T5); `guest size` is write-only | A cloned disk cannot be resized from a script; the clone keeps the source's disk size | §4 T5 |
| `hypervisor:true` is accepted and silently ignored for a non-matching architecture (T6) | `virtualize` on x86_64 is refused in the adapter as well as in the resolver — the value UTM reads back cannot be trusted | §4 T6 |
| `--hide` is broken (T7) | There is no headless UTM start; a start opens a window and that is logged, not hidden | §4 T7 |
| `utmctl`/`osascript` fail when the app is not up: `-609` while launching, `-1712` when wedged (T8) | Every call handshakes through `open -a UTM` and retries **once** | §4 T8 |
| `stop` can hang; `update configuration` needs a fully stopped VM (T9) | Stops poll for `stopped` and escalate to `by kill` only after a timeout; configuration writes refuse a non-stopped VM | §4 T9 |
| An unknown display hardware name is silently replaced (T11) | Only names UTM documents are used | §4 T11 |
| `stop --kill` can leave an orphan `qemu-*-softmmu` that UTM reports as stopped (T12) | A stop that leaves an orphan holding the VM's `-uuid` **fails, reported with the pid** — wargame never signals a process it did not spawn | §4 T12 |
| Windows 11's TPM 2.0 / Secure Boot toggles are absent from UTM 4.7.5's dictionary | Not scripted, not bypassed: `profile create` prints the one-time manual step | §6 |
| `input scan code` / `input keystroke` are an exit-0 no-op here | Console automation is not built on; the OS install is a human step | §2 (downgraded 2026-09-09) |
| `utmctl ip-address` returns several lines (IPv4, ULA, link-local) | The adapter selects IPv4, else a routable address; never link-local or loopback | §5 |

Minimum supported UTM is **4.3** (`MIN_UTM_VERSION`); **4.7.5** is the only
version verified (`VERIFIED_UTM_VERSION`), and `doctor` says so for anything else.

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
│   ├── wargame-tart/              # VmProvider + ProfileImageBuilder for Tart
│   ├── wargame-utm/               # VmProvider + ProfileImageBuilder for UTM   (new)
│   └── wargame-cli/               # binary `wargame`: clap CLI + ratatui TUI (depends on all)
├── profiles/
│   ├── macos-minimal-arm64.yaml   # built profiles (Tart)
│   ├── macos-re-arm64.yaml
│   ├── kali-arm64.yaml            # templates (UTM); copied with `profile init`
│   ├── kali-x86_64.yaml
│   ├── windows-x86_64.yaml
│   └── scripts/
│       ├── macos/{common.sh,reversing.sh}
│       ├── kali/common.sh
│       ├── windows/common.ps1
│       └── custom/{my-tools.sh,my-tools.ps1}   # commented examples
├── guest/
│   ├── bootstrap.sh               # trusted guest bootstrap, POSIX (macOS + Kali)
│   └── bootstrap.ps1              # trusted guest bootstrap, Windows/PowerShell
├── scripts/
│   ├── e2e-smoke.sh               # Tart/macOS smoke, fixture challenge
│   └── e2e-multi-os.sh            # any built profile, everything after `profile build`
├── docs/
│   ├── utm-compatibility.md       # the verified UTM contract (§1)
│   ├── verification.md            # the MVP's real-VM record
│   └── archive/                   # superseded plans
├── changes.d/                     # sacho changelog fragments (§12)
├── DESIGN.md, PLAN.md, MVP.md, README.md, CLAUDE.md
└── .claude/workflows/             # multi-agent build workflow
```

Dependency rule (NFR-05), enforced by Cargo:
`wargame-dreamhack`, `wargame-tart` and `wargame-utm` depend only on
`wargame-core`; none depends on another. Only `wargame-cli` wires them
together. A challenge-source change therefore cannot leak into a VM backend,
and the two VM backends cannot leak into each other.

---

## 3. `wargame-core`

### 3.1 Modules

```
core/
  domain/
    challenge.rs      Challenge, ChallengeSummary, ChallengeAsset, ChallengeRef ("dreamhack:11")
    profile.rs        Profile (YAML schema §6), VmSettings{BackendChoice, ExecutionMode},
                      GuestSpec{os, arch, distribution, user, password}, Resources,
                      PROFILE_IMAGE_PREFIX, validate_guest_user
    session.rs        Session (+ snapshot fields), SessionState (+ allowed transitions),
                      GuestConnection, GuestEndpoint, GuestShell, VmRef{backend,id,name},
                      NetworkMode, KeepPolicy
    compat.rs         Backend, SupportedCombination, SUPPORTED, resolve, ResolvedBackend,
                      ProviderCapabilities, check
    build.rs          BuildRecord, BuildSource, BuildStage, BuildPhase, InstallerMedia,
                      PublishedImage, VerificationResult, profile_hash, scripts_hash
  ports/
    wargame_provider.rs   trait WargameProvider
    vm_provider.rs        trait VmProvider, VmSpec, VmStatus, BackendStatus, BackendCheck,
                          ConsoleDescriptor, StartOptions, RuntimeInfo, ImageRef
    image_builder.rs      trait ProfileImageBuilder, trait BuildRecordRepository,
                          BuildRequest, InstallerRequest, GuestLink, EjectOutcome
    credential_store.rs   trait CredentialStore
    session_repo.rs       trait SessionRepository
    guest_transport.rs    trait GuestTransport (wait_ready, exec, exec_script, copy_in, interactive_shell_cmd)
    disk_space.rs         trait DiskSpace (free_bytes)
  app/
    backends.rs       BackendRegistry, TransportRegistry, resolve_backend,
                      provider_for_profile / provider_for_session / transport_for_session
    search.rs         SearchChallenges
    launch.rs         LaunchChallenge (the pipeline, emits LaunchEvent)
    lifecycle.rs      Stop / Resume / Delete / Keep / Inspect / List / Prune / reconcile
    console.rs        OpenSsh / OpenGui
    doctor.rs         doctor, doctor_profile, Check, CheckStatus
    profiles.rs       ProfileStore, ProfileOverview, ImageReadiness
    profile_build.rs  init / create / console / build, run_build_flow, BuilderRegistry,
                      image_status, launch_image, MIN_FREE_GIB, REBOOT_REQUIRED_EXIT_CODE
    profile_vm.rs     eject / stop / delete for the VMs a build record owns
  security/
    filename.rs       sanitize_filename(server_name) -> SafeName
    staging.rs        StagingDir (0700 dir, 0600 files, drop = delete unless keep_debug)
    redact.rs         Redactor for tracing layer (cookies, Authorization, passwords, key material)
    hash.rs           sha256 streaming writer
  infra/
    process.rs        trait ProcessRunner { run(Cmd) -> Output; spawn_detached(Cmd, log) -> Pid }
    fs.rs             AppDirs ($WARGAME_HOME, incl. builds_dir), disk_free_bytes(path), HostDiskSpace
    ssh.rs            SshGuestTransport (endpoint-keyed; POSIX or PowerShell wire format), GuestService
    guest_ops.rs      GuestOps, GuestCommand — the single OS-shape seam
    powershell.rs     quote, encode_command, PowerShellCommand, EXIT_CODE_MARKER
    keychain.rs       KeychainStore (security -i), FileStore (0600 json; tests / CI), MemoryStore
    json_repo.rs      JsonSessionRepository (sessions/<id>/metadata.json, atomic tmp+rename)
    json_build_repo.rs JsonBuildRepository (builds/<profile>/record.json, same discipline)
  config.rs           Config (TOML) with defaults, incl. [vm.tart] and [vm.utm]
  error.rs            WargameError (user-facing categories from PRD §23) + `hint()` text
  testing/            FakeVmProvider, FakeTransport, FakeRunner, MemoryRepo, MemoryBuildRepo
```

### 3.2 Domain types and the session snapshot

```rust
pub struct ChallengeRef { pub provider: String, pub id: String }        // "dreamhack:11"
pub struct ChallengeSummary { pub r#ref: ChallengeRef, pub slug: String, pub title: String,
                              pub categories: Vec<String>, pub difficulty: Option<String> }
pub struct Challenge { pub summary: ChallengeSummary, pub description: String, pub tags: Vec<String>,
                       pub assets: Vec<ChallengeAsset>, pub remote_endpoints: Vec<String>, pub source_url: String }

pub enum GuestOs { Macos, Linux, Windows }
pub enum GuestArch { Arm64, X86_64 }
pub enum BackendChoice { Auto, Tart, Utm }          // profile request; Default = Auto
pub enum ExecutionMode { Virtualize, Emulate }      // Default = Virtualize
pub enum NetworkMode { Isolated, Nat }
pub enum GuestShell { Posix, PowerShell }           // GuestShell::for_os(os)
pub struct Resources { pub cpu: u32, pub memory_mib: u64, pub disk_gib: u64 }

/// A VM as its backend knows it. `id` is the stable provider-side identity —
/// a UTM UUID, a Tart VM name — and `name` is only ever shown or
/// prefix-checked. A record written before `id` existed deserializes with
/// `id = name`, which is exactly what Tart addresses a VM by.
pub struct VmRef { pub backend: String, pub id: String, pub name: String }

/// How to reach a guest, minus the address.
pub struct GuestConnection { pub user: String, pub port: u16, pub shell: GuestShell,
                             pub key_path: Option<PathBuf> }
pub struct GuestEndpoint { pub host: IpAddr, pub port: u16, pub user: String,
                           pub key_path: Option<PathBuf>, pub shell: GuestShell }
```

`Session` carries the **execution snapshot** spec.md §3 FR-5 asks for, so no
lifecycle operation ever has to re-read a profile that may since have been
edited:

| Field | Meaning | Value in a pre-multi-OS record |
|---|---|---|
| `vm_provider` | the backend the launch resolved to | `"tart"` |
| `vm_ref: Option<VmRef>` | stable `(backend, id)` + display name | `id` filled from `name` |
| `guest_os` / `guest_arch` | the guest that was created | as recorded |
| `guest_distribution` | `kali` for a Linux guest | absent |
| `vm_mode: ExecutionMode` | how it was created | `virtualize` |
| `connection: GuestConnection` | user / port / shell / key | `admin`, 22, `posix`, host key |
| `workspace_root: String` | the guest path the challenge was placed under | `""` (taken from the profile) |
| `network`, `resources`, `keep`, `state`, `assets`, `runner_pid`, `guest_ip`, `last_error` | as before | — |

Every new field is `#[serde(default)]`, and the defaults are exactly the
single hard-wired behaviour the MVP had — `DEFAULT_GUEST_USER = "admin"`,
`DEFAULT_GUEST_PORT = 22`, `ExecutionMode::Virtualize`, Tart. A legacy record
therefore loads and runs unchanged, and is not rewritten on read (verified on
the real host, §10).

`Session::backend_name()` reads the backend off `vm_ref` first and falls back
to `vm_provider`; `Session::endpoint()` builds a `GuestEndpoint` from the
saved connection once `guest_ip` is known. Both exist so nothing downstream
re-derives routing from a profile.

**State transitions** (anything else is a bug and returns `InvalidTransition`):

| From | To | Meaning |
|---|---|---|
| `Creating` → `Downloading` → `Booting` → `Provisioning` → `Ready` | | the launch pipeline |
| `Ready` → `Running`; `Running` ⇄ `Stopped` | | up / down after a launch |
| `Stopped` → `Booting` | | resume |
| any state but `Deleted` | `Failed` | anything can fail |
| **any state but `Deleted`** | `Deleting` → `Deleted` | delete is terminal cleanup |

`Deleting` is reachable from **every** state except `Deleted`, not only from
the finished ones: `delete` is the last resort and has to work on whatever a
crash left behind — a launch killed mid-`Booting` as much as a session that
ran. `Deleting → Deleting` is not a transition (the `self == to` guard), so a
record already `Deleting` is an *interrupted* delete that `delete` **continues**
— it skips the transition and goes straight to removing the VM and the record.
That is what makes the recovery `prune` points at actually work.

`Ready` means bootstrap finished and the VM is up; `Running` is the same VM
after a resume. Both count as "up" for `ssh`/`gui`. The record can drift when
the VM stops without going through the CLI (a window is closed, the guest
shuts down, the runner dies): every lifecycle read (`sessions`, `inspect`,
`stop`, `resume`, `ssh`, `gui`) first **reconciles** an "up" record against
the backend and moves it to `Stopped` when the VM is reported stopped. A
`Stopped` session may be `gui`-started directly. A `Missing` VM is left alone
— that is delete/prune territory.

**`prune` ownership.** `prune` cleans up the VMs *this* `WARGAME_HOME` left
behind, and decides ownership from **records, never from a name prefix**
(spec.md §3 FR-5). A VM is a deletion candidate only when all four hold:

1. a session record **in this home** owns it — by the stable `(backend, id)` a
   launch wrote down, or by the `<prefix><session id>` name a launch that died
   between `vm.create` and the record save would have used (still
   record-derived: it needs a session id this home has a record for); **and**
2. that record no longer justifies the VM (`Failed`, `Deleting` or `Deleted`); **and**
3. the session is not `keep`-flagged; **and**
4. the backend reports the VM `Stopped`.

Everything else is left alone. The result is a `PruneReport { orphans,
running, unmanaged }`: owned leftovers that are still running are **reported
with the session to delete, never deleted**; prefix-matching VMs no record
here owns — another home's, a build VM, the user's own — are reported as
`UnmanagedVm { vm, backend, name }` so the CLI can name the right manual
remedy per backend, and are never touched. VMs a build record owns are skipped
entirely. After a real (non-dry-run) delete, `settle_record` finishes the
bookkeeping: a `Deleting`/`Deleted` record is removed (prune completed the
interrupted delete), a `Failed` record is kept — the failure is still worth
reading — with `vm_ref`, `runner_pid` and `guest_ip` cleared so it no longer
points at a VM that is gone.

### 3.3 Ports

```rust
#[async_trait] pub trait WargameProvider: Send + Sync {
    fn name(&self) -> &str;                                              // "dreamhack"
    async fn search(&self, q: &SearchQuery) -> Result<SearchPage>;
    async fn get_challenge(&self, id: &str) -> Result<Challenge>;
    async fn auth_state(&self) -> Result<AuthState>;                     // Anonymous | Authenticated { display }
    async fn login(&self, username: &str, password: Secret<String>) -> Result<()>;
    async fn download_asset(&self, ch: &Challenge, a: &ChallengeAsset, sink: &mut dyn AssetSink) -> Result<DownloadResult>;
    async fn healthcheck(&self) -> Result<ProviderStatus>;
}

#[async_trait] pub trait VmProvider: Send + Sync {
    fn name(&self) -> &str;                                              // "tart" | "utm"
    fn capabilities(&self) -> ProviderCapabilities;
    async fn healthcheck(&self) -> Result<BackendStatus>;                // available, version, isolated_network, detail
    async fn image_exists(&self, image: &ImageRef) -> Result<bool>;
    async fn pull_image(&self, image: &ImageRef) -> Result<()>;
    async fn create(&self, spec: &VmSpec) -> Result<VmRef>;              // clone + apply resources/network
    async fn start(&self, vm: &VmRef, opts: &StartOptions) -> Result<RuntimeInfo>;
    async fn wait_ip(&self, vm: &VmRef, timeout: Duration) -> Result<IpAddr>;
    async fn stop(&self, vm: &VmRef) -> Result<()>;
    async fn delete(&self, vm: &VmRef) -> Result<()>;
    async fn status(&self, vm: &VmRef) -> Result<VmStatus>;              // Running | Stopped | Missing
    async fn list(&self) -> Result<Vec<VmRef>>;                          // this app's namespace only
    async fn diagnostics(&self) -> Vec<BackendCheck> { Vec::new() }      // extra `doctor` rows
    fn storage_dir(&self) -> Option<PathBuf> { None }                    // volume a free-space check must measure
    fn console(&self, vm: &VmRef) -> ConsoleDescriptor;                  // Window { argv } | Vnc | None
}

pub struct VmSpec { pub name: String, pub image: ImageRef, pub resources: Resources,
                    pub execution: ResolvedBackend, pub network: NetworkMode }

#[async_trait] pub trait GuestTransport: Send + Sync {
    async fn wait_ready(&self, endpoint: &GuestEndpoint, timeout: Duration) -> Result<()>;
    async fn exec(&self, endpoint: &GuestEndpoint, argv: &[String]) -> Result<ExecResult>;
    async fn exec_script(&self, endpoint: &GuestEndpoint, script: &str) -> Result<ExecResult>;
    async fn copy_in(&self, endpoint: &GuestEndpoint, src: &Path, dest: &str) -> Result<()>;
    fn interactive_shell_cmd(&self, endpoint: &GuestEndpoint) -> Command;
}

pub trait DiskSpace: Send + Sync { fn free_bytes(&self, path: &Path) -> Result<u64>; }
pub trait CredentialStore: Send + Sync { /* get / set / delete */ }
```

Every `GuestTransport` method is keyed by a `GuestEndpoint`, never by a bare
`IpAddr`: the user, port, key and shell come from the profile at launch and
from the saved session afterwards, so nothing in the port can assume a
host-wide default user.

`VmProvider::storage_dir` exists because `$WARGAME_HOME` is *not* where every
backend writes disks. UTM is sandboxed and keeps bundles in its own container,
so a free-space check against the wargame home measures the wrong filesystem.

### 3.4 Launch pipeline (`app/launch.rs`)

Input: `LaunchRequest { challenge, profile, network, resources, extract, gui }`.
Output: a stream of `LaunchEvent` and the final `Session`.

```
1. preflight — every check before any side effect
   a. load profile
   b. resolve_backend(profile)                 → Incompatible (pure data; no backend contacted)
      …emit "profile <n>: kali/x86_64 on utm/emulate" so a long emulated boot is never a surprise
   c. compat::check(profile, backend.name(), backend.capabilities())   → Incompatible
   d. capabilities.supports_network(req.network)                       → NetworkUnavailable
   e. transports.for_shell(GuestShell::for_os(profile.guest.os))       → refuse an unwireable guest
   f. vm.healthcheck()                                                 → BackendUnavailable
      + isolated requested while the backend reports it unavailable    → NetworkUnavailable
   g. profile_build::launch_image(profile) + vm.image_exists(image)    → ImageNotBuilt / ImageMissing
   h. free space on the *backend's* storage volume ≥ disk_gib × 0.25   → InsufficientResources
   i. provider.get_challenge(id)                                       → ChallengeNotFound / ProviderUnavailable
   j. assets non-empty and auth_state == Anonymous                     → AuthenticationRequired
2. session record: id = "dh-<slug>-<6 hex>", state Creating, with the execution
   snapshot (§3.2), persisted BEFORE any VM exists (NFR-02)
3. vm.create(spec) → vm_ref persisted immediately
4. state Downloading: each asset → StagingDir, sha256 recorded in session.assets
5. state Booting: vm.start(headless unless gui) → pid (when the backend has one);
   vm.wait_ip(boot timeout for this backend+mode); transport.wait_ready(endpoint)
6. state Provisioning: workspace + bootstrap + digests + metadata + assets, then the
   bootstrap runs in the guest — every path and command from GuestOps (§3.8)
7. staging dir deleted (unless config.debug.keep_staging)
8. state Ready
On any error: state Failed with last_error; the VM is left in place so `wargame delete`
can clean it up, and the message names the session id.
```

Progress events: `Preflight`, `SessionCreated(id)`, `VmCreated`,
`Downloading{filename}`, `Downloaded{filename, sha256}`, `Booting`,
`GuestIp(ip)`, `Bootstrapping`, `Ready{ssh_hint}`, `Failed{error}`.

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

[vm.utm]
# utmctl = "/Applications/UTM.app/Contents/MacOS/utmctl"   # optional; tried first
vm_prefix = "wg-"                     # session VMs
build_prefix = "wg-build-"            # installation + published-image VMs
boot_timeout_secs = 1800              # emulate
virtualize_boot_timeout_secs = 300    # virtualize

[wargame.dreamhack]
base_url = "https://dreamhack.io"
credential_store = "keychain"   # keychain | file (file is for tests/CI only and warns)

[debug]
keep_staging = false
```

Every field is `#[serde(default)]`, so a partial (or absent) config is valid.
`VmConfig::boot_timeout(backend, mode)` is the single place a wait is derived
— Tart always 300 s, UTM 300 s virtualized and 1800 s emulated — and
`vm_prefix(backend)` / `build_prefix(backend)` are the single place a VM name
is derived, so a UTM launch can never be named from `[vm.tart]`.

`$WARGAME_HOME` overrides the root directory. Layout under it:
`config/config.toml`, `profiles/` (+ `profiles/.builtin/` pristine copies),
`sessions/<id>/metadata.json` (+ `run.log`), **`builds/<profile>/record.json`**,
`keys/id_ed25519{,.pub}`, `cache/`, `logs/wargame.log` (+
`logs/provision-<profile>.log`).

### 3.6 Security details

- **Filename**: take the last path component, reject empty/`.`/`..`, strip control chars and leading dots, limit to 255 bytes, fall back to `asset-<id>.bin`.
- **Staging**: `tempfile::Builder` under `$WARGAME_HOME/cache/staging/` with mode 0700; files 0600; written via a `.part` name and renamed only after the body ends cleanly and the SHA-256 is computed. Partial downloads never carry the final name.
- **Redaction**: a `tracing` layer that replaces values of fields named `cookie`, `authorization`, `password`, `sessionid`, `csrftoken`, and any string matching `sessionid=…` / `Bearer …`. Unit-tested.
- **Keychain**: service `wargame-vm`, account `dreamhack.session`. Value is a JSON blob `{cookies: [...], created_at}`. Written through `security -i` stdin, read with `find-generic-password -w`. Passwords are never persisted; only the resulting session cookies are.
- **Guest key**: `ssh-keygen -t ed25519 -N ''` into `$WARGAME_HOME/keys/` (0600) on first use. Tart builds install it over a one-time password login; a UTM guest is told to authorize it during the manual install. It never leaves the host except as the public half.
- **No host shell strings, anywhere.** Every external process is `Command::new(bin).args([...])` through `ProcessRunner`. AppleScript is a **trusted constant template** whose data arrives as `osascript` arguments read back through `on run argv`, so the script text never contains user data (§8). PowerShell data is quoted by `infra::powershell::quote` (single-quoted literal, `'` doubled) — deliberately *not* the POSIX `'\''` escape, which would leave a stray backslash in the guest's argument.
- **Secrets stay out of argv, logs and records.** `sshpass` reads the one-time build password from the `SSHPASS` environment variable; `profile_hash` never includes `guest.password`; build and session records carry no secret fields.
- **`guest.user` is validated when the profile is read.** It is interpolated into a sudoers line the user pastes **as root** and into the `user@host` an `ssh` argv carries, so anything outside `[A-Za-z0-9._-]` (starting with a letter, digit or `_`) is refused with `InvalidProfile` naming both the field and the profile. Rejection rather than escaping: an escaped name would not look like the account the user typed, and no real account name needs it. A profile refused this way *explains itself* instead of silently disappearing from `ProfileStore`.
- **Ownership floors in the adapters.** Application services decide ownership from records (§3.2); each backend also refuses, before running anything, to stop or delete a VM outside this app's namespaces — Tart: `[vm.tart].vm_prefix` or `PROFILE_IMAGE_PREFIX`; UTM: registered in `utmctl list` **and** inside `[vm.utm].vm_prefix`. The user's own `kindle-vm` is untouchable by construction.
- **Nothing from a challenge is opened on the host.** The host downloads, hashes and copies; extraction, listing and every safety check happen in the guest (§3.8).

### 3.7 Backend selection (`app/backends.rs`, `domain/compat.rs`)

Two registries and three lookups, so no application service ever branches on
a backend name.

```rust
pub struct BackendRegistry;    // every wired VmProvider, keyed by provider.name()
impl BackendRegistry { fn new(Vec<Arc<dyn VmProvider>>); fn get(&str); fn all(); fn names(); }

pub struct TransportRegistry;  // a GuestTransport per GuestShell
impl TransportRegistry { fn new(posix); fn with_powershell(t); fn for_endpoint(&GuestEndpoint); fn for_shell(GuestShell); }

pub fn resolve_backend(profile) -> Result<ResolvedBackend>;
pub fn provider_for_profile(ctx, profile) -> Result<(ResolvedBackend, Arc<dyn VmProvider>)>;
pub fn provider_for_session(ctx, session) -> Result<Arc<dyn VmProvider>>;
pub fn transport_for_session(ctx, session) -> Result<(GuestEndpoint, Arc<dyn GuestTransport>)>;
```

`AppContext` holds `backends: BackendRegistry` and `transports:
TransportRegistry` — there is no single `vm` or `transport` field left to
branch off.

**The rule.** A *new launch* resolves from the **profile**
(`provider_for_profile`). Everything afterwards — `resume`, `stop`, `delete`,
`ssh`, `gui`, `inspect` — routes from the **session's snapshot**
(`provider_for_session`, `transport_for_session`). Editing a profile or
changing a default backend can therefore never strand a running session, and
two sessions whose VMs share a name on different backends can never be
confused, because every key is `(backend, id)`. `prune` is the one command
that walks `backends.all()` rather than one session's backend, and it decides
what it may touch from records alone (§3.2).

**The support matrix is data** (`compat::SUPPORTED`), evaluated before any
backend is contacted:

| backend | guest | arch | mode |
|---|---|---|---|
| `tart` | macOS | arm64 | `virtualize` |
| `utm` | Linux (`distribution: kali`) | arm64 | `virtualize` |
| `utm` | Linux (`distribution: kali`) | x86_64 | `emulate` |
| `utm` | Windows | x86_64 | `emulate` |

Windows on arm64 is deliberately absent: spec.md §1 lists it as a follow-up,
and accepting it would promise a path nothing implements.

`resolve(profile)` matches `(os, distribution, arch, mode)` against that
table. `BackendChoice::Auto` — the default, and what every pre-multi-OS YAML
means — picks the single backend that supports the combination; an explicit
`tart`/`utm` must itself support it. **There is no runtime fallback**: an
unsupported request is `WargameError::Incompatible` carrying the request, a
reason that narrows step by step (guest → architecture → mode, so it names
the specific thing that cannot be done), and the full
`supported_combinations_table()`. A Linux profile with no
`guest.distribution` is told to set one rather than being told "Linux is
unsupported" next to a table full of Linux rows.

`resolve` is *pure*: it runs first in every flow that can create something
(launch preflight, `profile build`, `doctor --profile`), so an impossible
combination is refused before anything is created, cloned or started.

`ProviderCapabilities` is the second, independent check
(`compat::check(profile, backend_name, caps)`): the matrix says which backend
*should* run a profile, capabilities say whether the backend wired into *this
build* agrees. It gained `modes: Vec<ExecutionMode>` and `networks:
Vec<NetworkMode>` (both `#[serde(default)]`, defaulting to `[Virtualize]` and
`[Isolated, Nat]`), so a mode or network a backend cannot configure is
refused before a VM exists rather than silently downgraded.

| backend | `guest_os` | `guest_arch` | `modes` | `networks` |
|---|---|---|---|---|
| Tart | macOS | arm64 | virtualize | isolated, nat |
| UTM | Linux, Windows | arm64, x86_64 | virtualize, emulate | isolated, nat |

`ResolvedBackend { backend, mode, os, arch, distribution }` is what a launch
carries into `VmSpec.execution` and snapshots into the session. Its
`describe()`/`guest_label()`/`backend_label()` are the one vocabulary the
`profiles` table, the TUI picker, `doctor` and launch progress all render, so
the requested line and the supported table can be compared by eye.

### 3.8 Guest connection and the OS-shape seam

**One transport, endpoint-shaped.** `SshGuestTransport` serves every guest;
it selects its wire format from `GuestEndpoint.shell`:

- **POSIX** — `ssh [-i key] -p port user@host <argv, each element shell-quoted>`. Byte for byte what the MVP sent, so the macOS path `docs/verification.md` records as working is not re-derived.
- **PowerShell** — `powershell -NoProfile -NonInteractive -EncodedCommand <base64>`, the script being UTF-16LE + base64. Nothing about the command then depends on how `ssh`, `sshd` or the guest's default shell would re-split it: Windows paths, spaces, single and double quotes and Korean characters cross the wire intact. Files go over `scp`'s SFTP protocol with the destination path untouched.

`TransportRegistry` keeps one slot per shell. `wiring.rs` registers the same
adapter in both, but the registry is what makes a build with *no* PowerShell
transport refuse a Windows endpoint outright instead of opening a POSIX
connection that would quote its arguments wrongly — and launch and `profile
build` both check `for_shell(...)` in preflight, before a VM is created.

**The exit-code marker.** `ssh` exits with the *low byte* of the remote
status, so a Windows installer's `3010` ("reboot required") would arrive as
194. The generated PowerShell wrapper prints the true code on its own stderr
line (`wargame-exit-code:<n>`) and exits 255; the transport puts the real
value back and removes the line, so nothing downstream — logs included — ever
sees the marker. `restore_exit_code` is gated on **all three** of: a
PowerShell endpoint, `ssh`'s own 255, and a marker actually present. A POSIX
guest is never wrapped, so a POSIX command that exits exactly 255 while
printing a marker-shaped line cannot have its status or stderr rewritten.

**`GuestOps` is the single OS-shape seam** (spec.md §5 item 3). Application
services ask it for what they need and never test the guest OS themselves:

```rust
GuestOps::for_endpoint(&endpoint) -> GuestOps
  separator() join() trim_root() dir_dest()      // path shape
  mkdir_p() remove_tree() flush() probe()        // filesystem + readiness
  run_script() bootstrap_name() run_bootstrap()  // execution
```

Each returns a `GuestCommand`: either `Argv(Vec<String>)`, which the
transport quotes or encodes as literals, or `Script(String)`, a template this
crate owns whose data is already quoted for that shell. The distinction is
load-bearing on Windows: a PowerShell **cmdlet** binds a parameter only from
a bare `-Name` token, so `& 'New-Item' '-ItemType' 'Directory'` would bind
`-Path '-ItemType'` and fail. Cmdlets therefore go through `Script`, with
parameter names written out and only data quoted; native executables
(`powershell.exe`, `tar.exe`, `sc.exe`, …) go through `Argv`.

**Readiness is classified, never assumed.** `wait_ready` polls a shell probe
until the deadline and, on timeout, reports *which* service is missing —
`GuestService::{Ssh, Authentication, Shell}` — inside
`WargameError::GuestNotReady { service, detail, hint }`, together with how
long it waited, the timeout used, what to prepare in the guest (for a Windows
endpoint: OpenSSH Server, the sshd service, PowerShell as `DefaultShell`, the
UTM guest tools) and which config key raises the wait. A missing service is
never reported as success.

**The bootstrap pair's contract.** `guest/bootstrap.sh` (POSIX; bash 3.2, so
macOS and Kali run the same file) and `guest/bootstrap.ps1` (Windows
PowerShell 5.1) are embedded in the binary and staged per launch:

```
bash bootstrap.sh <workspace-dir> --extract=<true|false> [--require-digests]
bash bootstrap.sh --check-entry <archive-entry-name>          # 0 = safe, 1 = rejected (reason on stderr)

powershell -File bootstrap.ps1 -Workspace <dir> -Extract <true|false> [-RequireDigests]
powershell -File bootstrap.ps1 -CheckEntry <archive-entry-name>
```

Both do the same five things: create `<root>/<slug>/{incoming,work,notes,output}/`;
verify every transferred file against the host's digests; optionally unpack
known archives; summarize the metadata into `notes/README.md`; print a
banner. The host stages `incoming/.wargame-sha256` (`<sha256>  <filename>`
per line, so neither script has to parse JSON) and `incoming/.wargame.json`
(provider, challenge id, title, per-file digests), and always passes
`--require-digests` — it has just written the digest list, so a guest that
cannot find it must refuse rather than quietly skip verification.

Archive safety, identical on every guest OS: an entry is rejected for an
absolute path, a `..` segment, a drive letter or UNC prefix, backslash
traversal, `:`, a reserved Windows device name, a component ending in a dot
or space, or a control character. An archive containing a symbolic or hard
link entry is refused whole. **A refusal keeps the original archive**, creates
no destination directory at all, and is written to
`notes/archive-refusals.txt` (reset each run), so a rejection can never leave
a half-unpacked tree behind. Extraction is scoped to
`incoming/extracted/<name>/`. Nothing found in `incoming/` is ever executed —
extraction only, no dot-sourcing, no evaluation of guest data as code — and
nothing is extracted, mounted, previewed or run on the host.

### 3.9 Image builds (`app/profile_build.rs`, `domain/build.rs`, `ports/image_builder.rs`)

**The record.** One `BuildRecord` per profile at
`$WARGAME_HOME/builds/<profile>/record.json`, written with the same
tmp+rename discipline as a session record. It is what makes a half-finished
install resumable and what tells UTM which images exist at all.

| Field | Purpose |
|---|---|
| `profile`, `backend`, `os`, `arch`, `mode` | what this record is for |
| `source: BuildSource` | `Iso { path, sha256, verified_sha256 }` \| `UtmVm { uuid }` \| `TartBase { image }` |
| `installer_vm: Option<VmRef>` | the long-lived VM the OS was installed into |
| `installer_media: InstallerMedia` | `Unknown` \| `Attached` \| `Detached` |
| `phase: BuildPhase` | `Created` \| `AwaitingInstall` \| `Provisioning` \| `Verifying` \| `Publishing` \| `Ready` \| `Failed { stage, error }` |
| `image`, `previous_image`, `leaked_images` | the current published image, the one it replaced, and any an earlier deletion could not remove |
| `profile_hash`, `scripts_hash` | `needs_rebuild()`; `profile_hash` never includes `guest.password` |
| `verification: Vec<VerificationResult>` | every `tools.verify` command and its exit code |
| `last_error`, `cleanup_failures`, `created_at`, `updated_at` | diagnosis |

`verified_sha256` is a separate flag from `sha256` on purpose: an ISO the
user merely named a hash for is never reported as verified (FR-2). Every new
field is `#[serde(default)]`, so an older record loads.

**The flow is one function**, `run_build_flow`, with the backend half behind
`ProfileImageBuilder`:

```
prepare → clone_for_build → eject_installer_media → boot → connect
        → provision → verify → flush → shutdown → publish
```

`provision`, `verify`, `flush`, the provisioning log and all record keeping
live in core; a backend implements only what only it can do. `BuildObserver`
persists each stage the flow reaches from `Provisioning` onward — the earlier
stages say nothing a record should claim, since a UTM profile is still
`awaiting-install` until the guest answers and a rebuild's previous image is
still the launchable one.

- **provision** copies the profile's `scripts/` tree into `~/wg-provision` and runs each `provision.scripts` entry *in the guest*, through `GuestOps`, logging every command and exit code to `logs/provision-<profile>.log`. A script that exits **`REBOOT_REQUIRED_EXIT_CODE = 3010`** is not a failure: the build waits for SSH to drop, waits for it to come back, and carries on with the *next* script. Any other non-zero code fails the build with the last 15 lines of output.
- **verify** runs each `tools.verify` entry as argv (whitespace-split, no shell) and records `{command, exit_code}`. The first failure ends the build.
- **flush** removes `wg-provision` and `sync`s the guest — best-effort, never fatal.
- **shutdown** must be *clean*; a guest that does not stop is never published.
- **publish** is the backend's own step: UTM copies the VM to `wg-build-image-<profile>`; Tart's provisioned, stopped VM already *is* `wargame-profile-<name>`, so publishing only names it.

**The eject step** (2.5, between clone and boot) exists because UEFI boots the
removable drive first: an installed guest that still has its installer CD
comes back up in the installer, never reaches SSH, and hands every copy of
itself the same CD. It is idempotent, and its outcome is a five-way model
rather than a boolean:

| `EjectOutcome` | Meaning | Effect on the build |
|---|---|---|
| `Detached(n)` | n removable drives written out of the drive list | continues; record → `Detached` |
| `NothingAttached` | there was nothing to detach | continues; record → `Detached` |
| `NotApplicable` | this backend has no installer media (Tart), or the VM was adopted with `--from-vm` | continues; record unchanged |
| `Blocked(reason)` | the VM is not stopped, so its configuration cannot be written | **continues**, printing the reason and `profile stop` + `profile build`; the record still says the media is attached |
| `EveryDriveRemovable(reason)` | **refused**: every drive is removable, so detaching them would leave the VM with nothing | continues, saying so; `profile eject` fails with a hint that does *not* say "stop the VM", because stopping changes nothing |

Two rules keep this safe. A `BuildSource::UtmVm` record **never ejects at
all** — wargame attached no media to a VM the user installed, so whatever
removable drives it has are theirs. And the AppleScript itself counts the
survivors and refuses before either write when none would remain, because
`removable` cannot be changed after a drive is created, so an adopted VM's
boot disk can legitimately carry the flag. A failed eject is attributed to
`BuildStage::Booting`: it is what makes the boot land in the installed system.

**Preservation rules.**

- `BuildPhase::Ready` is reached **only** after verification *and* a clean shutdown *and* a successful publish. A guest still being installed stays `AwaitingInstall` — a wait, not a failure — and a build whose `connect` step times out on a backend that uses an installation VM goes *back* to `AwaitingInstall` rather than being marked broken.
- The new image is written into the record **before** the old one is deleted, so a crash in between leaves two images, never none. `discard_replaced_images` then deletes the replaced one; anything it cannot delete is tracked in `leaked_images` so `prune` keeps skipping it and the next build tries again. A backend whose image id is stable across builds republishes the same id, and then there is no old image to delete.
- A failed build keeps the previous image launchable — **except on Tart**. Tart's image *is* the VM, so `prepare` must delete `wargame-profile-<name>` before cloning; during a rebuild the previous image does not exist, and if the rebuild fails, `record.image` names an image that is gone. It fails loudly rather than silently: `launch` checks `image_exists` and refuses with `ImageMissing`, and `profiles`/`doctor` show the profile as not built (both ask the backend, not just the record). Fixing it properly means publish-to-temp-and-rename, which Tart has no verb for — it would be a second full disk copy per build. Recorded as a known limitation, not implemented.

**Free space.** `profile create` refuses only below a documented floor,
`MIN_FREE_GIB = 8` GiB — a guest disk is sparse and grows as it is used, so
demanding the profile's nominal `disk_gib` up front would refuse installs that
fit comfortably. Between the floor and the nominal size it creates the VM and
prints one line saying how much is free, what the profile asks for, and that
the disk grows. The measurement is taken on the volume the **backend** writes
to (`VmProvider::storage_dir`), falling back to `$WARGAME_HOME` when that
directory cannot be read — a sandboxed container this process may not stat is
not a reason to refuse a create. `InsufficientResources` carries its own hint,
which for a create says to free space on *that* volume and that `disk_gib` is
a ceiling rather than a reservation.

**`profile_vm.rs`** owns the three lifecycle verbs for the VMs a *build
record* owns — separate from the build flow, because they act on the
installation VM rather than on a build. All three route through the record's
`(backend, uuid)`, never a name, and refuse a backend with no installation VM
(Tart) before anything runs.

- **eject** — detaches the installer media; idempotent; refuses a running VM with `profile stop` then `profile eject`.
- **stop** — the *same* `VmProvider::stop` a session stop uses, so the whole UTM contract applies unchanged (resume a paused VM, `stop by force`, poll for `stopped`, escalate to `by kill` after the timeout, refuse while a T12 orphan holds the VM). Already stopped is a no-op; a VM UTM no longer has is reported, not an error.
- **delete** — the installation VM, every image the profile's builds published (current, previous and leaked), and the record. It prints each VM with its status and asks first (`--force` skips the question; a non-TTY refuses). It **refuses while any of them is running**. The record is deleted **last**, because it is the only thing that knows these VMs exist, so one that could not be removed stays reachable for a retry.

`DeletePlan`/`DeleteReport`/`StopReport`/`EjectReport` carry `OwnedVm { role,
vm, status }` so every message can say *which* VM — installation VM or
published image — it is talking about.

---

## 4. `wargame-dreamhack`

Unchanged by the multi-OS work.

```
dreamhack/
  client.rs     reqwest::Client with cookie_store, rustls, UA "wargame-vm/<ver>"; loads cookies from CredentialStore
  api.rs        raw endpoint functions returning serde structs
  model.rs      serde structs mirroring the API (only fields we need; `#[serde(default)]` liberally)
  normalize.rs  api::* -> core::Challenge / ChallengeSummary   (slug = repository or slugified title)
  provider.rs   impl WargameProvider
  auth.rs       login flow: GET / (csrf cookie) → POST /api/v1/auth/login/ → persist cookie jar
  download.rs   POST …/download/ with X-CSRFToken + Referer; handles shapes (1)/(2) from §1
fixtures/       search.json, detail.json, search_page.json, login_401.json, download_401.json
```

Asset discovery: the detail response does not list files. `Challenge.assets`
gets one **logical asset** whose filename is a placeholder until the server
names the real one, so `download_asset` may return a different filename and
the launch pipeline uses the returned name.

Errors: 401/403 → `AuthenticationRequired`; 404 → `ChallengeNotFound`;
unexpected JSON → `ProviderChanged { what }`; network → `ProviderUnavailable`.

---

## 5. `wargame-tart`

The macOS/arm64 backend, and now one of two. Nothing about its behaviour
changed except where the multi-backend seams touch it.

```
tart/
  provider.rs   impl VmProvider using ProcessRunner (argv arrays); vm name = "<prefix><session-id>"
  args.rs       pure functions building argv for each op (unit-tested against FakeRunner)
  list.rs       serde for `tart list --format json`
  image.rs      ImageRef; profile image local name = "wargame-profile-<name>"
  build.rs      TartImageBuilder: impl ProfileImageBuilder
  console.rs    ConsoleDescriptor::Window { argv }; OpenGui = stop headless → spawn_detached(run with window)
```

Capabilities: `guest_os=[macos]`, `guest_arch=[arm64]`,
`modes=[virtualize]`, `networks=[isolated, nat]`, `graphical_console=true`,
`ssh=true`, `snapshots=false`, `copy_in=true`, `remote=false`.

- Tart's stable id **is** its name, so `create`/`list` return `VmRef::new("tart", name)` (`id == name`) and old session records keep pointing at the same VM.
- `start` spawns `tart run …` detached (new session/process group, stdout+stderr → `sessions/<id>/run.log`) and returns the pid. `stop` calls `tart stop` and polls until the VM really stopped. `status` uses `tart list --format json`.
- **Namespace floor** (added with the multi-backend work): `stop`/`delete` refuse a VM whose name is outside `[vm.tart].vm_prefix` *and* `PROFILE_IMAGE_PREFIX` before `tart` is invoked at all. Two namespaces rather than one, because discarding a published image legitimately deletes a `wargame-profile-*`.
- `TartImageBuilder` implements the §3.9 flow: `prepare` pulls the base image (and deletes the previous build of the same name — see the preservation exception), `clone_for_build` clones and sizes, `boot` starts headless on NAT, `connect` installs the host public key over a one-time `sshpass` login (secret only in `SSHPASS`, never argv, never logged) and waits for the key to be accepted, `shutdown` stops and confirms, `publish` names the result, `cleanup_failed_build` stops and deletes a partially built image so a later `launch` cannot mistake it for a finished one. `eject_installer_media` is left at the default `NotApplicable`.
- `SshGuestTransport::new(runner, default_key_path)` carries **no** guest user: it comes from `profile.guest.user` at launch and from the session's saved connection afterwards.

---

## 6. Profiles and templates

`profiles/*.yaml` are embedded in the binary with `include_str!` and also
seeded into `$WARGAME_HOME/profiles/` so users can edit and add. Schema, with
the multi-OS additions marked `NEW`:

```yaml
apiVersion: wargame.vm/v1
kind: Profile
metadata: { name: kali-custom, description: "Kali with custom analysis tools" }
vm:                                   # NEW, optional
  backend: utm                        # auto | tart | utm      (default: auto)
  mode: emulate                       # virtualize | emulate   (default: virtualize)
guest:
  os: linux                           # macos | linux | windows
  distribution: kali                  # NEW, optional; only meaningful for os: linux
  arch: x86_64                        # arm64 | x86_64
  user: kali                          # required; validated as an account name ([A-Za-z0-9._-], §3.6)
  password: admin                     # optional; only used during a Tart image build
image:
  base: ""                            # Tart: the OCI/base image. UTM: empty — the source is the build record
resources: { cpu: 4, memory_gib: 8, disk_gib: 80 }
provision:
  scripts: [scripts/kali/common.sh]
tools:
  verify: [python3 --version, gdb --version]
workspace:
  root: /home/kali/wargame            # Windows: 'C:\wargame'
```

A profile with **no `vm:` section is a Tart/virtualize profile**, which is
exactly what every pre-multi-OS YAML means — the two macOS profiles are
unchanged files. An empty UTM `image.base` means "this profile is only
buildable and launchable once a build record exists" (spec.md §3 FR-2).

**Shipped profiles.**

| Profile | Backend / mode | Guest | Scripts | Kind |
|---|---|---|---|---|
| `macos-minimal-arm64` | tart / virtualize | macOS arm64, user `admin` | `macos/common.sh` | built profile |
| `macos-re-arm64` | tart / virtualize | macOS arm64, user `admin` | `macos/common.sh`, `macos/reversing.sh` | built profile |
| `kali-arm64` | utm / virtualize | Kali arm64, user `kali` | `kali/common.sh` | template |
| `kali-x86_64` | utm / emulate | Kali x86_64, user `kali` | `kali/common.sh` | template |
| `windows-x86_64` | utm / emulate | Windows 11 x86_64, user `wargame`, workspace `C:\wargame` | `windows/common.ps1` | template |

The three UTM entries are **templates**: they have no image and no build
record until the user runs `profile init` + `profile create` + a manual OS
install + `profile build`. Both Kali templates carry a comment above
`resources:` explaining that an emulated guest uses every core it is given,
so two installs or builds at once make both crawl (a 10-core host running
them together logged guest soft-lockups and a load average of 24); `profile
create` prints the same in one line.

**Seeding never overwrites a user's file** (spec.md §3 FR-3). Each built-in
path is compared against a pristine copy under `profiles/.builtin/`: a file
that still equals what the previous binary seeded is refreshed, so an
unedited recipe follows the binary; a file the user edited is left alone
forever; a path the built-in set does not name at all — anything the user
added under their own name — is never written, and no pristine copy is kept
for it. `profile init <template> --name <new>` copies the template YAML,
rewrites only `metadata.name` and the `provision.scripts` entries (comments
survive verbatim, because the comments are what make it a template), and
gives the copy its **own** `scripts/<new name>/` directory, so a later binary
re-seeding the built-in scripts can never touch the user's edits. Nothing is
written until the rewritten YAML parses as the profile that was asked for.

**Tool lists and their verify lines.** Each provisioning script's header
carries a table of `tool → install source → verify command`, and the verify
column *is* the profile's `tools.verify` list, so an install and its proof
are written next to each other:

| Guest | Installed from | Tools |
|---|---|---|
| macOS | Homebrew | git, python, uv, curl, wget, jq, vim, tree (+ `reversing.sh`: Ghidra, openjdk@21, LLVM, lld, binutils, hexyl, file) |
| Kali | Kali's own apt repositories, over the build's NAT | python3, python3-pip, python3-venv, git, curl, unzip, file, gdb, binutils, build-essential |
| Windows | the `OpenSSH.Server` Windows capability + `winget` | PowerShell (ships), OpenSSH Server, Python 3, Git, 7-Zip |

`tools.verify` entries are **argv**, whitespace-split, with no shell and no
pipes — a lesson from the MVP's reversing image (`… | grep …` failed with
`Bad argument: grep`). On Windows there is a second rule: every token reaches
the guest as a quoted literal, so **each entry must be a native executable**
(`powershell.exe`, `sc.exe`, `python.exe`, `git.exe`, `7z.exe`). A PowerShell
*cmdlet* there would have its parameter names bound as positional values and
fail. Inside a `.ps1` provisioning script there is no such restriction — that
runs as a whole script.

---

## 7. `wargame-cli`

```
cli/
  cli.rs         clap: search|find, open|show|info, launch|run, sessions|ls|ps, session inspect,
                 ssh, gui, stop, resume|start, delete|rm, keep, prune, doctor [--profile],
                 profiles, profile {init|create|console|eject|stop|delete|rm|build},
                 auth [login|status|logout], completions <shell>; no subcommand → TUI
  main.rs        dispatch; resolves session references; prints "Next:" hints after every command
  wiring.rs      builds Config, AppDirs, providers, registries, repos, transports (composition root)
  output.rs      human tables + `--json`
  tui/           search / problem / launch / sessions screens over the same application services
```

**Profile verbs** (spec.md §4). `init` copies a template under a new name and
refuses a name that already exists. `create` makes the installation VM from
`--iso <path>` or `--from-vm <uuid>` (mutually exclusive; `--sha256` verifies
the ISO before anything is created), prints the VM's UUID and what to prepare
in the guest, and is **idempotent for the same source** — re-running it
re-prints the guidance and creates nothing, while pointing it at a *different*
source is an error naming how to make a new profile instead. `console` starts
the installation VM and brings its window up, and says that the VM stays
running until `profile stop`. `eject` takes the installer disc out. `stop`
stops the installation VM. `delete` (alias `rm`, `--force`/`-f`) removes the
VMs the build record owns and the record. `build` runs §3.9's flow.

`create` success means "the installation VM exists"; `build` success means "a
verified, cleanly stopped image was published". Neither is claimed for the
other.

**`wargame profiles`** (and the TUI's profile picker) render one
`ProfileOverview` per profile: the guest (`kali/x86_64`), the backend and mode
(`utm/emulate`), and the image state — all from the same
`ResolvedBackend`/`ImageReadiness` pair `doctor` uses, so the three views can
never disagree (spec.md §3 FR-1). A profile the matrix cannot resolve carries
its reason instead of a backend, rather than being hidden.

**Session references.** Every session-taking command accepts a *reference*,
resolved by `lifecycle::resolve_session`: the exact id, else a unique id
prefix, else a unique case-insensitive substring of the id or challenge
title; several matches are an `AmbiguousSession` error listing the
candidates. Omitting the reference (allowed everywhere except `delete`) means
the most recently created session.

`wargame ssh <session>` execs `ssh` (replacing the process), landing in
PowerShell for a Windows session. `wargame gui` opens the backend's console
(§0). `wargame delete` on a `Keep` session requires `--force` or interactive
confirmation (NFR-07).

**`wargame prune`** applies §3.2's ownership rule across **every** wired
backend and prints three kinds of line:

```
would delete tart/wg-…
still running, left alone: utm/wg-… — remove it with: wargame delete <id>
2 VM(s) carry this app's name prefix but no record here owns them — another
wargame home's, or your own — so they were left alone:
  tart/wg-other — if you want it gone: tart delete wg-other
  utm/wg-build-kali — if you want it gone: delete "wg-build-kali" in the UTM app (right-click the VM ▸ Delete)
```

`--dry-run` changes nothing at all, records included.

**`wargame doctor`** runs every host check; **`wargame doctor --profile
<name>`** runs only the checks *that* profile needs, so a macOS/Tart profile
never reports UTM as a problem. A check has **three** outcomes —
`CheckStatus::{Pass, Fail, NotSetUp}`, rendered `✓` / `✗` / `·` and exposed
as `status: "pass" | "fail" | "not-set-up"` in `--json` beside the existing
`ok`.

| Row | Global | `--profile` | Notes |
|---|---|---|---|
| `platform` | ✓ | | macOS on Apple Silicon |
| `profile:<name>` | | ✓ | what the profile resolved to (`backend utm / mode emulate / Linux(kali)/x86_64`) |
| `vm-backend:<backend>` | one per wired backend | just this profile's | version or the reason it is unavailable |
| `network:isolated:<backend>` | one per wired backend | the network this profile would use | **named per backend** — a single row would silently describe whichever backend ran last |
| `mode:<mode>:<backend>` | | ✓ | with the support table as its hint when it fails |
| `utm:utmctl` / `utm:version` / `utm:automation` | | ✓ (from `diagnostics()`) | discovered path and where it came from; version against 4.3 minimum / 4.7.5 verified; whether `osascript` can drive UTM |
| `profile-image:<name>` | one per profile | ✓ | see below |
| `disk-space` / `disk-space:<name>` | 20 GiB floor | this profile's `disk_gib` | |
| `guest-shell:<shell>` | | ✓ | is a transport wired for this guest |
| `ssh`, `scp`, `ssh-keygen`, `security`, `sshpass`, `wargame-home`, `provider:<name>`, `guest-key` | ✓ | ssh/scp only | `sshpass` is build-only |

`profile-image:<name>` is where the three-way status earns itself. A profile
**nobody has built here** is `not-set-up`, not a failure: on a macOS-only host
the three shipped UTM templates would otherwise show three permanent ✗ next
to `doctor` exiting 0, and a ✗ you are told to ignore teaches you to skim
past the ✗ that matters. `Fail` is kept for everything that *is* wrong — an
image that went missing after a build, an image the profile has outgrown
(`rebuild needed`), a build that stopped partway, a backend that cannot
answer at all. The distinction is derived from the build record, not from
whether a profile happens to be one of the shipped templates, so a user's own
never-created profile is treated the same way. Nothing is hidden: those rows
still appear with their hint, listed after the failures under "N not set up
yet (nothing is wrong)". `doctor`'s exit code stays 0 deliberately — it is a
report, and the rows now carry the distinction the exit code could not.

The `network:isolated:utm` row never says a plain "ok": its detail is
`mechanism verified (UTM host = vmnet-host, host gets .1 on the guest
segment); guest isolation unverified`, because the mechanism is proven and
the guest-side blocking is not (§10).

---

## 8. `wargame-utm`

The UTM backend: VM creation, cloning, configuration, start/status/stop/
delete, console and the build-time operations, with no dependency on
Dreamhack and no knowledge of sessions. Every claim below traces to a row of
[`docs/utm-compatibility.md`](docs/utm-compatibility.md); that document is
the evidence and is not duplicated here.

```
utm/
  automation.rs  Automation — every external invocation: utmctl, osascript, `open -a UTM`, one `ps`
  provider.rs    UtmProvider — the VmProvider port + create_installer_vm / clone_vm /
                 attach_iso / replace_iso / eject_removable_drives / open_console; ImageCatalog
  build.rs       UtmImageBuilder (impl ProfileImageBuilder), RecordImageCatalog
  config.rs      UtmConfig, architecture(), network_mode(), display_hardware(),
                 BUNDLED_UTMCTL, UTM_APP, UTM_VM_DIRECTORY, MIN_UTM_VERSION, VERIFIED_UTM_VERSION,
                 ISOLATED_NETWORK_DETAIL
  console.rs     ConsoleDescriptor::Window { [utmctl, start, <uuid>] }
```

### 8.1 `Automation` — the only place a process runs

Two rules, both straight from §1:

- **AppleScript templates are trusted constants.** Every value that comes from a profile, a session or the user — names, UUIDs, paths, sizes, a stop method — is passed as an `osascript` *argument* and read back inside the script through `on run argv`. The script arrives on stdin (`osascript - <args>`), so the script text never contains user data: there is nothing to escape and no host shell string is ever built. A test pins the property itself — a name full of quotes and non-ASCII reaches `osascript` verbatim as one argv element and never appears in the script text.
- **Failures are matched by error *code*, never by prose.** `utmctl`'s exit code does not report Apple-Event failures (T1), so a call fails when stderr carries `Error from event:` or `Error:` whatever the exit code says; and codes are matched as *numbers* (`-609` must not be found inside `-6091`, nor inside a UUID or path that happens to contain those digits) or by ASCII-only prefixes, because UTM writes its messages with typographic apostrophes.

**The `-609` handshake (T8).** `utmctl` and `osascript` fail — with exit 0 —
when the app is not up: `-609` while it is launching, `-1712` when it is
wedged. `ensure_app_running` does `open -a UTM` and waits for `utmctl version`
to answer; every call path goes through it, and any call that comes back with
one of those two codes clears the "app is ready" flag, re-handshakes and
retries **exactly once**. Both sides are covered by fixtures captured from the
real failure.

**UUID-only addressing.** `duplicate` returns nothing (T2) and display names
are not unique — `virtual machine named` silently picks the first match (T3)
— so every id-addressed script is written as a `repeat` over `virtual
machines` comparing `id of vm`, and a clone's new UUID is recovered by a set
difference of the registry before and after, with the name only as a
tie-breaker.

**The `run_on_vm` sentinel.** Such a script *falls through* its loop when no
VM carries the UUID, and returns `wargame-utm:vm-not-found` with exit 0 — a
perfectly successful `osascript` run. Treating that as success would turn "the
VM is gone" into a silent no-op reported as done: a `set_network_mode` that
never ran while an isolated session boots on whatever the source VM had, a
delete that deleted nothing. So the sentinel is an **error**
(`WargameError::VmNotFound`) everywhere except `status`, where "missing" is
the answer rather than a failure. `eject` uses the same mechanism to return a
second sentinel, `wargame-utm:no-surviving-drive`.

The surface: `make_vm`, `duplicate`, `list`, `find`, `status`, `start`,
`stop`, `delete`, `set_hardware`, `set_network_mode`, `set_display`,
`attach_iso`, `replace_iso`, `eject_removable_drives`, `guest_ip`,
`find_orphan_qemu`, `activate_app`, `ensure_app_running`, `version`,
`automation_probe`, `utmctl_located`. `utmctl` is discovered in FR-6's order:
`[vm.utm].utmctl` → `PATH` → `/Applications/UTM.app/Contents/MacOS/utmctl`,
paths with spaces included.

### 8.2 `UtmProvider`

Capabilities: `guest_os=[linux, windows]`, `guest_arch=[arm64, x86_64]`,
`modes=[virtualize, emulate]`, `networks=[isolated, nat]`,
`graphical_console=true`, `ssh=true`, `snapshots=false`, `copy_in=true`,
`remote=false`. macOS guests stay on Tart: UTM's QEMU backend does not run
them and the working macOS path is deliberately untouched.

**The ownership floor.** `owned_vm` is the gate in front of every mutation: a
VM is acted on only when it is **registered in `utmctl list`** *and* its
display name is inside `[vm.utm].vm_prefix`. A UUID that is not registered is
`Ok(None)` — gone, nothing to do. A UUID that *is* registered but sits outside
the prefix belongs to the user and is refused. `list()` filters to the same
namespace, so `prune` never even sees anything else.

**Status mapping**, exhaustive over UTM's enumeration so a new state in a
future release is a compile error rather than a silent "stopped":

| UTM status | `VmStatus` | Why |
|---|---|---|
| `starting`, `started`, `pausing`, `paused`, `resuming` | `Running` | all have a live backend; a paused VM still occupies its resources |
| `stopped`, `stopping` | `Stopped` | `stopping` is stopped-in-progress; no configuration may be written in it either |
| not registered / `unavailable` backend | `Missing` | do not act on this VM |

**Stopping** (T9 + T12). A paused VM is resumed first (`stop` refuses it),
then `stop by force`, then a **poll for `stopped`** rather than trusting the
verb, then `by kill` only after `stop_timeout`, then a poll again. Finally,
T12: if a `qemu-*-softmmu` process still carries this VM's `-uuid`, the stop
**fails**, naming the pid, with a `Try:` line offering `ps -o pid,ppid,command
-p <pid>` and `kill -9 <pid>`. **wargame never signals it** — the process
belongs to UTM's `QEMUHelper`, and killing something we did not spawn is
exactly what FR-5 forbids. `delete` stops first (or, for an already-stopped
VM, checks for the orphan anyway: deleting under one is how bundles get
corrupted).

**Starting.** A start enforces the session's network **while the VM is
stopped** — the only state UTM accepts a configuration write in — so an
`isolated` session can never come back up on NAT because something changed the
VM in the app. UTM runs QEMU under its own XPC service, so there is no child
process of ours: `RuntimeInfo { pid: None }`. There is no headless start
(`--hide` is broken, T7); a request for headless is logged as a warning saying
the window will open, because a caller that asked for headless is getting
something else.

**Addresses.** `wait_ip` polls `utmctl ip-address`, which reads from the QEMU
guest agent and returns several lines (IPv4, ULA IPv6, link-local); the
adapter selects IPv4, else a routable address, and never link-local or
loopback. On timeout the error says the guest needs `qemu-guest-agent`
installed and running — before that, an installing or half-provisioned guest
never answers — and the hint names the run log and the two config keys that
raise the wait.

**Networks.** `isolated` → UTM's `host` mode (`-netdev vmnet-host`), `nat` →
`shared` (`-netdev vmnet-shared`). `emulated` (libslirp) is deliberately
unreachable: it is not isolated, and port forwarding is not implemented. UTM
holds the vmnet entitlements itself, so `host` needs no sudo and no helper —
unlike Tart's Softnet path. `healthcheck` reports `isolated_network:
Some(true)` with a detail that says the *mechanism* is verified and the
**guest-side isolation is not** (§10).

**Console.** There is no `utmctl console` and no working `attach` (both print
`WARNING: attach command is not implemented yet!`, exit 0 and stream
nothing), so "open the console" is `utmctl start <uuid>`, which starts or
resumes the VM and foregrounds the app — never with `--hide`.

**Build-time operations.**

- `create_installer_vm(name, os, arch, mode, resources, iso)` — blank disk + installer ISO on a removable drive + UEFI + a **display** (a VM made by `make` has none at all, so it would have no window to install through). Returns `VmRef { backend: "utm", id: <UUID>, name }`.
- `clone_vm(source_id, new_name)` — `duplicate` + registry set difference. **Refuses a running source**: copying a running VM captures a torn disk.
- `attach_iso` / `replace_iso` — a second removable drive, or swapping the first one.
- `eject_removable_drives` — writes the drive *list* back without its removable entries (UTM has no "remove a drive" verb), which needs a **stopped** VM (T9) and needs each surviving drive to echo its own `id`, or UTM *adds* a drive instead of keeping one. Zero removable drives is a success that writes nothing; a VM whose drives are all removable is refused before either write.
- `open_console` — start if stopped, else just activate the app.
- `storage_dir()` — `~/Library/Containers/com.utmapp.UTM/Data/Documents`, where the qcow2 actually lands, when it exists (it does not until UTM has run once). The directory is `drwx------`: the adapter never tries to *read* it, only to measure its volume.

**Known limitations, all recorded rather than worked around.**

| Limitation | Consequence |
|---|---|
| TPM 2.0 and Secure Boot are absent from UTM 4.7.5's dictionary | Windows 11 needs a one-time manual step in UTM ▸ Settings ▸ QEMU; `profile create` prints it for a `windows-*` profile. Not bypassed — no Autounattend or LabConfig trick is implemented |
| A cloned disk cannot be resized from a script (T5) | The clone keeps the source image's disk size; `resources.disk_gib` is logged as not applied |
| No port forwarding (`protocol:TCP` is rejected, T4) | Guests are reached on their own address; `GuestConnection.port` exists for when that changes |
| No headless start (T7) | A UTM VM's window opens; wargame says so instead of pretending otherwise |
| Any UTM call launches the app | Inherent to the automation surface: a `doctor`, a `status`, a `prune` on a host with UTM installed will bring UTM up |

`UtmImageBuilder` implements §3.9's flow over these operations, and
`RecordImageCatalog` answers `image_exists` from the build records — UTM has
no image registry of its own, so "which images are ready" is a question only
the app's own records can answer. Without it every UTM profile would report
"not ready" forever.

---

## 9. Testing

| Layer | How | Runs where |
|---|---|---|
| core unit | filename sanitization, staging rename semantics, redaction, state transitions, the support matrix and resolver, capability checks, config defaults, `GuestOps` shapes, PowerShell quoting/encoding/exit-code marker, build-record phases and hashes, the build flow's step order and preservation rules, prune ownership, doctor statuses, launch pipeline with fake provider/vm/transport | `cargo test`, `nix flake check` |
| dreamhack fixtures | parse `fixtures/*.json`, normalization, 401 → `AuthenticationRequired`, unexpected shape → `ProviderChanged`, CSRF header construction, `Set-Cookie` redaction | same |
| tart argv | every operation's argv against `FakeRunner`; `tart list` JSON parsing; the namespace floor | same |
| utm | every script's *text* (addressed by UUID, never by name; the eject's no-survivor guard precedes both writes), error-code matching against captured stderr fixtures, `utmctl list`/`ip-address` parsing against real captured output, the `-609` retry on both the `utmctl` and `osascript` sides, the build flow's ordering | same — `FakeRunner` only; **no UTM, no Apple Events** |
| cli | `wargame --help`, subcommand parsing, `doctor --json` on a fake environment, `sessions --json` on a temp `WARGAME_HOME` | same |
| VM integration (Tart) | `cargo test -p wargame-tart --features e2e -- --ignored`: clone vanilla → run → ssh → copy fixture zip → bootstrap → verify → delete | opt-in, on the Mac |
| VM integration (UTM) | `crates/wargame-utm/tests/e2e.rs` — `#[ignore]`d and doubly env-gated, against an already-published image | opt-in, on the Mac |
| E2E smoke (macOS/Tart) | `scripts/e2e-smoke.sh [profile]`: fixture challenge (`WARGAME_PROVIDER=fixture`), launch → clone → download → boot → SSH → bootstrap → Ready → verify → delete | opt-in |
| E2E multi-OS | `scripts/e2e-multi-os.sh <profile> [challenge]`: takes any profile whose image is **already built** and verifies everything after `profile build` — image readiness from `profiles --json`, the session record's backend/mode/os/arch/network, the guest's `uname -m`, the workspace tree, file digests, then cleanup | opt-in |

Neither e2e script installs an OS or builds an image; both refuse to start if
the image is not there, and both delete **only the sessions they created**
(the trap is armed from a snapshot of the session ids taken *before* the
launch, so a failure between "the VM is up" and "we know its id" still takes
the VM with it). They deliberately run against the real `$WARGAME_HOME`,
because the guest SSH key the images trust and the build records both live
there — so anything that was already in the home must survive. Installation
VMs and published images are never stopped, modified or deleted by either.

Plain `cargo test` reaches neither Tart nor UTM: everything runs on
`FakeRunner` and the in-memory fakes, and the non-VM suite runs on Linux too.
`$WARGAME_VM=fake` swaps the in-memory `FakeVmProvider` into **both** backend
slots (with the real UTM capability set mirrored onto the `utm` one) so the
whole CLI, including a UTM profile, can be exercised with no VM at all.

---

## 10. Verification posture

What follows is the state **at the time of writing** (2026-09-09, HEAD
`44f8d05`, with a real-VM acceptance run still in progress). The MVP's own
real-VM record — the macOS launches, the isolated-network proof, the
Dreamhack download, `gui`, and the thirteen bugs those runs found — is
[`docs/verification.md`](docs/verification.md); the UTM contract's evidence is
[`docs/utm-compatibility.md`](docs/utm-compatibility.md).

**Verified on real VMs**

- **AC-01 (no regression of the macOS/Tart path): PASS.** Both macOS profiles launched, stopped, resumed and deleted on the isolated network with the multi-OS binary; a legacy session record loaded as tart/arm64/virtualize/`admin` and was never rewritten; the user's own `kindle-vm` and pre-existing session were untouched.
- **Kali installs.** Both guests were installed for real through UTM: arm64 `virtualize` and x86_64 `emulate`, each with `kali`, passwordless sudo, SSH and the QEMU guest agent. Boot → SSH measured at ~25 s virtualized and ~48–68 s emulated, so the boot timeouts are not the risk; installation itself took ~1 h 20 m and ~10 h respectively. **AC-02 and AC-03 are supported by these runs** (x86_64 with the hypervisor off, arm64 virtualized).
- **The UTM automation contract** in §1/§8: every row of `docs/utm-compatibility.md`'s operation matrix was executed on this host, its §7 records both Kali guests booting (`hypervisor:true` → `-accel hvf` on arm64, `hypervisor:false` → `-accel tcg` on x86_64), and the eject script's *read* path was re-confirmed against the two installed guests with a byte-identical drive readback before and after.
- **AC-12 (the automated gate).** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `nix flake check` (build/fmt/clippy/tests) and `nix build` have been run green at each step of this branch, and plain `cargo test` reaches neither Tart nor UTM (§9), which is the "no UTM required" half of that criterion.

**Not verified at the time of writing**

- **Windows, end to end.** No Windows ISO exists on this host, so `profile create` → manual install → `profile build` → `launch` for a Windows guest is **unverified**, and `guest/bootstrap.ps1` has never run in a real guest. Everything Windows-side is exercised only against fakes and script-text assertions.
- **Guest-side network isolation on UTM.** UTM's `host` mode is `-netdev vmnet-host`, the same primitive Tart's verified isolated path uses, and that much is confirmed; whether the *guest* is actually cut off from the LAN and the internet needs an installed guest and has not been measured. `doctor` and `healthcheck` say exactly this rather than reporting "ok", and **AC-10 stays unverified for UTM** (it is verified for Tart in `docs/verification.md`).
- **The eject's write branch.** The refusal and no-op paths are proven live; `set drives of c` + `update configuration` on a VM that *does* have a removable drive has not yet been executed by this code.
- **AC-04 through AC-09 and AC-11 on the UTM guests** are in progress in the acceptance run: `profile build` end to end on both Kali guests, `--from-vm` cloning with the source preserved, a user script added and rebuilt, mixed-backend lifecycle after a config change, fault injection for the preservation rules, and the archive-escape/injection sample. Their *automated* halves are covered by §9's suites; the real-VM halves are not claimed here.

The rule this document follows, and the reports it draws on follow: an AC that
could not be exercised because of an external condition is recorded
**unverified**, never simulated and never counted as passed. `docs/verification.md`
is the place the per-AC results land.

---

## 11. Nix

`flake.nix` provides for `aarch64-darwin` (and `x86_64-linux` for cargo-only checks):

- `packages.default` = `rustPlatform.buildRustPackage` (name `wargame`, `cargoLock.lockFile = ./Cargo.lock`, `doCheck = true`).
- `apps.default` = `wargame`.
- `devShells.default` = rustc, cargo, clippy, rustfmt, rust-analyzer, cargo-nextest, jq, shellcheck, openssh, plus `tart` and `sshpass` on Darwin.
- `checks.{build,fmt,clippy,tests}`.

Runtime binaries (`tart`, `ssh`, `security`) are resolved from PATH at run
time; the package wraps `wargame` with `makeWrapper --prefix PATH :
${tart}/bin` so `nix run` works without Homebrew.

**UTM is not a Nix dependency.** It is a macOS *application* the user installs
(`brew install --cask utm`), reached through `utmctl` and Apple Events, and
nothing in the flake pretends otherwise. That is the reason for the split
`doctor` rows: a host with Tart and no UTM is a perfectly good host for the
macOS profiles, and registering the UTM backend costs nothing there —
`UtmProvider` only looks for `utmctl` when it is asked to do something, so a
missing UTM degrades the `utm` slot alone.

---

## 12. Process

**Changelog.** Entries are [sacho](https://sacho.dev/) fragments under
`changes.d/`, one file per user-visible change, written for the people who run
`wargame` (no type names, module paths, or how-hard-it-was narration) and
committed together with the code they describe. `CHANGELOG.md` is **never**
edited by hand: its unreleased region is generated, and `sacho release`
compiles the fragments into a dated section.

```sh
mise install                     # once: installs the pinned sacho (mise.toml)
sacho add <topic-name>           # new fragment, then write the entry in it
sacho fmt && sacho check         # normalize and validate before committing
```

Config lives in `sacho.toml`; see `CLAUDE.md`. A change with no user-visible
effect — a refactor, a test, a document like this one — gets **no fragment**.

**Documents.** `MVP.md` is the product spec, `README.md` owns usage, this file
is the design record, `docs/verification.md` is the real-VM record and
`docs/utm-compatibility.md` is the verified external contract. Superseded
plans live under `docs/archive/`. Source comments cite this file by section
number, which is why §1–§7 and §9 keep their MVP meanings (see the header).

---

## 13. Out of scope, and the seams left for it

Not built (kept as seams): Hyper-V or any remote agent, macOS x86_64
emulation, Windows on arm64, `.utm` bundle import, automatic remote image
downloads, an OCI registry for profile images, VNC consoles, snapshot UI,
port forwarding, browser login, multiple wargame providers, a general
unattended-OS-install engine.

The seams that make those additions cheap:

- `VmProvider` + `BackendRegistry` + the `SUPPORTED` table — a third backend is a crate, a registration in `wiring.rs` and rows in the matrix; no application service changes.
- `ProfileImageBuilder` — a backend's build steps, with ordering, provisioning, verification and record keeping already owned by core.
- `GuestOps` + `GuestShell` + `TransportRegistry` — a new guest OS shape is a `GuestOps` arm and a transport, not a branch in `launch`.
- `ConsoleDescriptor::Vnc` exists unused, for the console type that is not built yet; `ImageRef` is an opaque per-backend string (a Tart image name, a UTM UUID), so a backend with a different notion of "image" needs no port change.
- `GuestConnection.port` and `GuestEndpoint.port` are threaded through everything, so per-session forwarded ports need no schema change when a backend can allocate them.

Known limitations recorded rather than fixed, each with its reasoning
in place: Tart cannot preserve its previous image across a failed rebuild
(§3.9); `prune` can only ever delete leftovers of sessions this home still has
records for, so a VM whose record was deleted is reported as unmanaged forever
(§3.2); UTM cannot resize a cloned disk, cannot script TPM/Secure Boot, and
brings its app to the foreground on any call (§8).
