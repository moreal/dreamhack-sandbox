# wargame-vm MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> This plan is executed by the multi-agent workflow described in §"Execution workflow" at the bottom: one Sonnet implementer per task, one reviewer per task, sequential commits between stages.

**Goal:** Ship the `wargame` CLI/TUI that searches Dreamhack, launches a disposable Tart macOS VM from a declarative profile, injects challenge files safely, and manages session lifecycle, reproducibly built with a Nix flake.

**Architecture:** Hexagonal Rust workspace. `wargame-core` owns domain types, ports (traits), application services, security helpers and infra adapters. `wargame-dreamhack` and `wargame-tart` implement the two ports and depend only on core. `wargame-cli` is the composition root with clap commands and a ratatui TUI that call the same application services.

**Tech Stack:** Rust 2021 (nixpkgs rustc 1.97), tokio, reqwest (rustls, cookies), serde/serde_json/serde_yaml/toml, clap 4 (derive), ratatui 0.29 + crossterm 0.28, sha2, tempfile, tracing, thiserror, async-trait, secrecy, time, rand, fs2, rpassword. Nix flake for build/dev/check. External runtime tools: `tart`, `ssh`, `scp`, `ssh-keygen`, `security`, `sshpass` (build only).

**Spec:** `DESIGN.md` (implements `MVP.md`). Executors read both.

**Status (2026-09-05):** all nine tasks executed by `.claude/workflows/mvp-build.js` and merged on `feat/mvp`; real-VM verification and the fixes it forced are recorded in `docs/verification.md`.

## Global Constraints

- Host: macOS on Apple Silicon (`aarch64-darwin`); tests (non-e2e) must also pass on `x86_64-linux` via `cargo test` so `nix flake check` is portable. Anything macOS-only (Keychain, Tart) is behind a trait with a fake used in tests.
- No shell strings for subprocesses: `Command::new(bin).args([...])` only.
- No secret ever appears in argv, logs, session metadata, or config files (DESIGN §3.6).
- Host never extracts, previews, mounts, or executes challenge artifacts (MVP §4.1).
- `wargame-dreamhack` must not depend on `wargame-tart` and vice versa (NFR-05). Cargo enforces it: their `Cargo.toml` list only `wargame-core` from the workspace.
- All user-facing errors go through `WargameError` and carry a `hint()` (DESIGN §3.1, MVP §23).
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --workspace` must pass at the end of every task.
- Dependencies are pre-declared in Task 0; implementers must not add crates without stating why in their report (Cargo.lock is frozen for reproducibility; adding a crate is allowed only if genuinely needed, and then `Cargo.lock` must be regenerated inside `nix develop`).
- Do not commit from inside implementer/reviewer agents; the workflow's commit step does it.
- All paths below are relative to the repository root.

---

## File map

| Path | Responsibility | Task |
|---|---|---|
| `flake.nix`, `flake.lock`, `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `crates/*/Cargo.toml` | build system, workspace, dependency pins | 0 |
| `crates/wargame-core/src/domain/{challenge,profile,session,compat}.rs` | domain types + state machine + compatibility | 1 |
| `crates/wargame-core/src/ports/{wargame_provider,vm_provider,guest_transport,credential_store,session_repo}.rs` | traits everything else implements | 1 |
| `crates/wargame-core/src/{error,config}.rs` | user-facing errors, TOML config | 1 |
| `crates/wargame-core/src/security/{filename,hash,staging,redact}.rs` | safe download plumbing | 2 |
| `crates/wargame-core/src/infra/{process,fs,keychain,json_repo,ssh}.rs` | subprocess runner, dirs, credential stores, session repo, ssh transport | 2 |
| `crates/wargame-core/src/testing/*.rs` | fakes: `FakeRunner`, `FakeVmProvider`, `FakeWargameProvider`, `FakeTransport`, `MemoryStore`, `MemoryRepo` | 2 |
| `crates/wargame-dreamhack/src/*` + `fixtures/*` | Dreamhack adapter | 3 |
| `crates/wargame-tart/src/{args,list,image,provider,console}.rs` | Tart adapter | 4 |
| `crates/wargame-core/src/app/{search,launch,lifecycle,console,doctor,profiles}.rs` | application services | 5 |
| `crates/wargame-tart/src/build.rs`, `profiles/*.yaml`, `profiles/scripts/macos/*.sh`, `guest/bootstrap.sh` | image build + profiles + guest bootstrap | 6 |
| `crates/wargame-cli/src/{main,cli,wiring,output,fixture_provider}.rs` | CLI + composition root | 7 |
| `crates/wargame-cli/src/tui/*.rs` | TUI | 8 |
| `README.md`, `scripts/e2e-smoke.sh`, `crates/wargame-tart/tests/e2e.rs`, flake wrapper | docs + integration | 9 |

---

### Task 0: Scaffold workspace and Nix flake (done by the orchestrator, not an agent)

**Files:**
- Create: `flake.nix`, `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `crates/wargame-core/{Cargo.toml,src/lib.rs}`, `crates/wargame-dreamhack/{Cargo.toml,src/lib.rs}`, `crates/wargame-tart/{Cargo.toml,src/lib.rs}`, `crates/wargame-cli/{Cargo.toml,src/main.rs}`
- Create: `crates/wargame-dreamhack/fixtures/{search.json,detail.json,search_page.json}` (sanitized captures)

**Produces:** a workspace where `nix develop -c cargo test --workspace` and `nix build` succeed with stub crates; every dependency below is already in `Cargo.lock`.

Workspace dependencies (exact crates):

```toml
[workspace.dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "fs", "io-util", "time", "sync"] }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_yaml = "0.9"
toml = "0.8"
thiserror = "2"
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }
secrecy = { version = "0.10", features = ["serde"] }
sha2 = "0.10"
hex = "0.4"
tempfile = "3"
time = { version = "0.3", features = ["serde", "formatting", "parsing", "macros"] }
rand = "0.8"
fs2 = "0.4"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "cookies", "json", "stream", "gzip"] }
futures-util = "0.3"
url = "2"
clap = { version = "4", features = ["derive", "env"] }
ratatui = "0.29"
crossterm = "0.28"
rpassword = "7"
comfy-table = "7"
libc = "0.2"
```

- [ ] Write the files, run `nix develop -c cargo test --workspace`, run `nix build`, commit `chore: scaffold workspace and nix flake`.

---

### Task 1: Core domain, ports, errors, config

**Files:**
- Create: `crates/wargame-core/src/domain/mod.rs`, `domain/challenge.rs`, `domain/profile.rs`, `domain/session.rs`, `domain/compat.rs`
- Create: `crates/wargame-core/src/ports/mod.rs`, `ports/wargame_provider.rs`, `ports/vm_provider.rs`, `ports/guest_transport.rs`, `ports/credential_store.rs`, `ports/session_repo.rs`
- Create: `crates/wargame-core/src/error.rs`, `crates/wargame-core/src/config.rs`
- Modify: `crates/wargame-core/src/lib.rs` (declare modules, re-export `WargameError`, `Result`)
- Tests: inline `#[cfg(test)]` modules in each file.

**Interfaces (Produces — later tasks rely on these exact names):**

```rust
// error.rs
#[derive(Debug, thiserror::Error)]
pub enum WargameError {
    #[error("authentication required for {provider}")] AuthenticationRequired { provider: String },
    #[error("authentication failed: {reason}")] AuthenticationFailed { reason: String },
    #[error("provider {provider} unavailable: {reason}")] ProviderUnavailable { provider: String, reason: String },
    #[error("provider {provider} response changed: {what}")] ProviderChanged { provider: String, what: String },
    #[error("challenge {0} not found")] ChallengeNotFound(String),
    #[error("download failed for {filename}: {reason}")] DownloadFailed { filename: String, reason: String },
    #[error("hash or write failure: {0}")] HashOrWrite(String),
    #[error("incompatible environment: {0}")] Incompatible(String),
    #[error("image {image} missing for profile {profile}")] ImageMissing { profile: String, image: String },
    #[error("insufficient resources: {0}")] InsufficientResources(String),
    #[error("VM backend {backend} unavailable: {reason}")] BackendUnavailable { backend: String, reason: String },
    #[error("VM creation failed: {0}")] VmCreationFailed(String),
    #[error("guest boot timed out after {0:?}")] GuestBootTimeout(std::time::Duration),
    #[error("guest bootstrap failed: {0}")] GuestBootstrapFailed(String),
    #[error("copy into guest failed: {0}")] CopyFailed(String),
    #[error("network unavailable: {0}")] NetworkUnavailable(String),
    #[error("cleanup failed: {0}")] CleanupFailed(String),
    #[error("session {0} not found")] SessionNotFound(String),
    #[error("invalid session state transition {from:?} -> {to:?}")] InvalidTransition { from: SessionState, to: SessionState },
    #[error("profile {0} not found")] ProfileNotFound(String),
    #[error("invalid profile {name}: {reason}")] InvalidProfile { name: String, reason: String },
    #[error("configuration error: {0}")] Config(String),
    #[error("credential store error: {0}")] CredentialStore(String),
    #[error("{0}")] Io(#[from] std::io::Error),
    #[error("{0}")] Other(String),
}
impl WargameError { pub fn hint(&self) -> Option<String>; }   // e.g. ImageMissing -> "Run: wargame profile build <profile>"
pub type Result<T> = std::result::Result<T, WargameError>;
```

Domain (DESIGN §3.2) — implement exactly:

```rust
// challenge.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)] pub struct ChallengeRef { pub provider: String, pub id: String }
impl ChallengeRef { pub fn parse(s: &str) -> Result<Self> /* "dreamhack:11"; bare number → provider "dreamhack" */; }
impl Display for ChallengeRef // "dreamhack:11"
pub struct ChallengeSummary { pub r#ref: ChallengeRef, pub slug: String, pub title: String, pub categories: Vec<String>, pub difficulty: Option<String> }
pub struct Challenge { pub summary: ChallengeSummary, pub description: String, pub tags: Vec<String>, pub assets: Vec<ChallengeAsset>, pub remote_endpoints: Vec<String>, pub source_url: String }
pub struct ChallengeAsset { pub id: String, pub filename: String, pub size: Option<u64>, pub descriptor: serde_json::Value, pub expected_sha256: Option<String> }
pub fn slugify(title: &str) -> String   // lowercase, [a-z0-9-], collapse dashes, max 40 chars, "challenge" if empty

// profile.rs
pub enum GuestOs { Macos, Linux, Windows }      // serde rename_all = "lowercase"
pub enum GuestArch { Arm64, X86_64 }             // serde: "arm64", "x86_64"
pub struct Resources { pub cpu: u32, pub memory_mib: u64, pub disk_gib: u64 }
pub struct Profile { pub name: String, pub description: String, pub guest: GuestSpec, pub image: ImageSpec, pub resources: Resources, pub provision: ProvisionSpec, pub tools: ToolsSpec, pub workspace: WorkspaceSpec }
pub struct GuestSpec { pub os: GuestOs, pub arch: GuestArch, pub user: String, pub password: Option<String> }
pub struct ImageSpec { pub base: String }
pub struct ProvisionSpec { pub scripts: Vec<String> }
pub struct ToolsSpec { pub verify: Vec<String> }
pub struct WorkspaceSpec { pub root: String }
impl Profile { pub fn from_yaml(s: &str) -> Result<Profile>; pub fn image_name(&self) -> String /* "wargame-profile-<name>" */ }
// YAML schema is DESIGN §6: apiVersion must be "wargame.vm/v1", kind "Profile"; memory_gib in YAML → memory_mib in struct.

// session.rs
pub enum NetworkMode { Isolated, Nat }           // serde lowercase; FromStr
pub enum KeepPolicy { Discard, Keep }
pub enum SessionState { Creating, Downloading, Booting, Provisioning, Ready, Running, Stopped, Failed, Deleting, Deleted }
impl SessionState { pub fn can_transition_to(self, to: SessionState) -> bool; pub fn is_up(self) -> bool /* Ready|Running */; pub fn is_terminal(self) -> bool /* Deleted */ }
pub struct SessionAsset { pub filename: String, pub sha256: String, pub size: u64 }
pub struct VmRef { pub backend: String, pub name: String }
pub struct Session { /* fields per DESIGN §3.2 */ }
impl Session { pub fn new_id(provider_short: &str, slug: &str, rand6: &str) -> String /* "dh-<slug>-<rand6>" */;
               pub fn transition(&mut self, to: SessionState) -> Result<()>; }
// transitions per DESIGN §3.2 table; Failed allowed from every non-terminal state; Stopped→Booting for resume.

// compat.rs
pub struct ProviderCapabilities { pub guest_os: Vec<GuestOs>, pub guest_arch: Vec<GuestArch>, pub graphical_console: bool, pub ssh: bool, pub snapshots: bool, pub copy_in: bool, pub remote: bool }
pub fn check(profile: &Profile, backend_name: &str, caps: &ProviderCapabilities) -> Result<()>  // Err(Incompatible(msg)) with the MVP §25 message shape
```

Ports (DESIGN §3.3) — copy the signatures verbatim from DESIGN.md §3.3 into the five port files, plus:

```rust
pub struct SearchQuery { pub text: String, pub category: Option<String>, pub page: u32, pub page_size: u32 }
pub struct SearchPage { pub items: Vec<ChallengeSummary>, pub next_page: Option<u32>, pub total: u64 }
pub enum AuthState { Anonymous, Authenticated { display: String } }
pub struct ProviderStatus { pub reachable: bool, pub auth: AuthState, pub detail: String }
pub struct DownloadResult { pub files: Vec<DownloadedFile> }     // one asset may yield several files
pub struct DownloadedFile { pub filename: String, pub path: PathBuf, pub sha256: String, pub size: u64 }
#[async_trait] pub trait AssetSink: Send { async fn begin(&mut self, server_filename: &str) -> Result<()>; async fn write(&mut self, chunk: &[u8]) -> Result<()>; async fn finish(&mut self) -> Result<DownloadedFile>; }
pub struct ImageRef(pub String);
pub struct VmSpec { pub name: String, pub image: ImageRef, pub resources: Resources }
pub struct StartOptions { pub network: NetworkMode, pub headless: bool, pub log_path: PathBuf }
pub struct RuntimeInfo { pub pid: Option<u32> }
pub enum VmStatus { Running, Stopped, Missing }
pub struct BackendStatus { pub available: bool, pub version: Option<String>, pub detail: String }
pub enum ConsoleDescriptor { Window { argv: Vec<String> }, Vnc { url: String }, None }
pub struct ExecResult { pub status: i32, pub stdout: String, pub stderr: String }
#[async_trait] pub trait SessionRepository: Send + Sync { async fn save(&self, s: &Session) -> Result<()>; async fn load(&self, id: &str) -> Result<Session>; async fn list(&self) -> Result<Vec<Session>>; async fn delete(&self, id: &str) -> Result<()>; fn session_dir(&self, id: &str) -> PathBuf; }
```

Config (DESIGN §3.5): `Config::load(path: Option<&Path>) -> Result<Config>` (missing file → defaults), `Config::default()`, serde with `#[serde(default)]` on every struct.

- [ ] **Step 1:** Write failing tests: `ChallengeRef::parse("dreamhack:11")`, `parse("11")`, `parse("")` errors; `slugify("Out Of Bound!!")=="out-of-bound"`; `Profile::from_yaml` on the DESIGN §6 sample maps `memory_gib: 8` → `memory_mib == 8192`, rejects wrong `apiVersion`; every allowed transition passes and `Ready→Creating` fails; `compat::check` accepts macos/arm64 and rejects windows/x86_64 with a message containing "supports:"; `Config::load(None)` gives `default_network == Isolated`; `Config` parses the DESIGN §3.5 TOML.
- [ ] **Step 2:** Run `cargo test -p wargame-core` → compile failures/red.
- [ ] **Step 3:** Implement the modules above.
- [ ] **Step 4:** `cargo test -p wargame-core`, `cargo clippy -p wargame-core --all-targets -- -D warnings`, `cargo fmt` → green.

---

### Task 2: Core security helpers, infra adapters, and test fakes

**Files:**
- Create: `crates/wargame-core/src/security/{mod,filename,hash,staging,redact}.rs`
- Create: `crates/wargame-core/src/infra/{mod,process,fs,keychain,json_repo,ssh}.rs`
- Create: `crates/wargame-core/src/testing/{mod,fake_runner,fake_vm,fake_wargame,fake_transport,memory_store,memory_repo}.rs` (public module, behind no feature flag so downstream crates can use it in tests)
- Modify: `crates/wargame-core/src/lib.rs`

**Interfaces (Consumes Task 1; Produces):**

```rust
// security/filename.rs
pub fn sanitize_filename(server_name: &str, fallback_id: &str) -> String   // DESIGN §3.6 rules; fallback "asset-<id>.bin"
// security/hash.rs
pub struct Sha256Writer<W: Write> { .. }  impl Write; fn finish(self) -> (W, String /*hex*/, u64 /*bytes*/)
pub fn sha256_file(path: &Path) -> Result<(String, u64)>
// security/staging.rs
pub struct StagingDir { pub path: PathBuf, keep: bool }
impl StagingDir { pub fn create(root: &Path, keep: bool) -> Result<Self> /* root/staging/<random>, mode 0700 */; pub fn sink(&self, asset_id: &str) -> StagingSink; }
impl Drop for StagingDir { /* remove_dir_all unless keep */ }
pub struct StagingSink { .. }   // impl AssetSink: begin() opens "<safe>.part" mode 0600; write() streams into Sha256Writer; finish() fsyncs, renames to "<safe>", returns DownloadedFile; drop without finish removes the .part
// security/redact.rs
pub fn redact(s: &str) -> String   // masks: "sessionid=…", "csrftoken=…", "Bearer …", "password=…", "-----BEGIN … PRIVATE KEY-----…-----END", values after "Cookie:"/"Authorization:" (case-insensitive) → "<redacted>"
pub fn init_tracing(verbose: bool, log_file: Option<&Path>)  // fmt layer whose event formatter runs `redact` on the message; RUST_LOG/env-filter respected
// infra/process.rs
pub struct Cmd { pub program: String, pub args: Vec<String>, pub env: Vec<(String,String)>, pub stdin: Option<Vec<u8>>, pub cwd: Option<PathBuf> }
pub struct Output { pub status: i32, pub stdout: String, pub stderr: String }
#[async_trait] pub trait ProcessRunner: Send + Sync {
    async fn run(&self, cmd: &Cmd) -> Result<Output>;                            // captures both streams; logs program+args via tracing::debug
    async fn spawn_detached(&self, cmd: &Cmd, log_path: &Path) -> Result<u32>;   // new session (setsid via pre_exec on unix), stdout+stderr appended to log_path, stdin null; returns pid
    fn which(&self, program: &str) -> Option<PathBuf>;
    fn pid_alive(&self, pid: u32) -> bool;                                       // libc::kill(pid, 0)
}
pub struct RealRunner;
// infra/fs.rs
pub struct AppDirs { pub root: PathBuf }
impl AppDirs { pub fn resolve() -> Result<Self> /* $WARGAME_HOME else ~/Library/Application Support/wargame-vm (macOS) else ~/.local/share/wargame-vm */;
               pub fn config_file(&self)->PathBuf; pub fn profiles_dir(&self)->PathBuf; pub fn sessions_dir(&self)->PathBuf; pub fn cache_dir(&self)->PathBuf; pub fn logs_dir(&self)->PathBuf; pub fn keys_dir(&self)->PathBuf; pub fn ensure_layout(&self)->Result<()> /* mkdir -p all, 0700 */ }
pub fn disk_free_bytes(path: &Path) -> Result<u64>   // fs2::available_space
// infra/keychain.rs
pub struct KeychainStore { runner: Arc<dyn ProcessRunner>, service: String }   // "wargame-vm"; uses `security -i` with stdin script lines; get → `find-generic-password -a <key> -s <service> -w` via run (argv has no secret); set → stdin "add-generic-password -U -a <key> -s <service> -w <value>\n" (value escaped with quotes); delete → "delete-generic-password -a <key> -s <service>"
pub struct FileStore { path: PathBuf }   // JSON map, file mode 0600, warns via tracing::warn at construction ("plaintext credential store")
// infra/json_repo.rs
pub struct JsonSessionRepository { root: PathBuf }   // impl SessionRepository; sessions/<id>/metadata.json; atomic write (tmp + rename); list sorted by created_at desc; load of missing → SessionNotFound
// infra/ssh.rs
pub struct SshGuestTransport { runner: Arc<dyn ProcessRunner>, user: String, key_path: PathBuf }
// impl GuestTransport: base args = ["-i", key, "-o","StrictHostKeyChecking=no","-o","UserKnownHostsFile=/dev/null","-o","ConnectTimeout=5","-o","LogLevel=ERROR","-o","BatchMode=yes"]
// wait_ready: loop `ssh … user@ip true` every 3s until success or timeout → GuestBootTimeout
// exec: `ssh … user@ip -- <argv shell-escaped>` (single-quote escaping helper `shell_quote`); copy_in: `scp -r … <src> user@ip:<dest>`; interactive_shell_cmd: std::process::Command for `ssh -t …`
pub fn shell_quote(s: &str) -> String
```

Fakes (`testing/`): `FakeRunner` records every `Cmd` (`calls: Mutex<Vec<Cmd>>`) and answers from a FIFO/`responder: Box<dyn Fn(&Cmd)->Output>`; `FakeVmProvider`, `FakeWargameProvider` (returns a fixture `Challenge` with one asset whose bytes are `b"PK\x03\x04fake"`), `FakeTransport` (records exec/copy calls, `ready: bool`), `MemoryStore` (CredentialStore), `MemoryRepo` (SessionRepository). All configurable via `Arc<Mutex<…>>` fields so app tests can assert calls and inject failures (`fail_on: Option<&'static str>`).

- [ ] **Step 1:** Tests first: `sanitize_filename("../../etc/passwd","7")=="passwd"`, `("", "7")=="asset-7.bin"`, `(".hidden","7")=="hidden"`, control chars stripped, 300-byte name truncated to 255; `StagingSink` writes `.part` then final name and sha256 matches `sha2` of the bytes; dropping a sink mid-write leaves no `.part`; `StagingDir` drop removes dir unless keep; `redact` masks all patterns and leaves normal text; `KeychainStore::set` sends the secret through stdin and never in argv (assert via `FakeRunner`); `JsonSessionRepository` round-trips a `Session` and lists newest first; `SshGuestTransport::exec` argv has the base options and `shell_quote("a'b")=="'a'\\''b'"`; `AppDirs::resolve()` honours `WARGAME_HOME`.
- [ ] **Step 2:** Run → red. **Step 3:** Implement. **Step 4:** `cargo test -p wargame-core`, clippy, fmt → green.

---

### Task 3: Dreamhack provider crate

**Files:**
- Create: `crates/wargame-dreamhack/src/{lib,client,api,model,normalize,provider,auth,download}.rs`
- Create: `crates/wargame-dreamhack/fixtures/{login_401.json,download_401.json}` (search/detail fixtures exist from Task 0)
- Tests: `crates/wargame-dreamhack/tests/fixtures.rs` (parsing/normalization), inline unit tests for header construction.

**Interfaces (Consumes Task 1 ports + `CredentialStore`; Produces):**

```rust
pub struct DreamhackConfig { pub base_url: Url, pub user_agent: String }
pub struct DreamhackProvider { .. }
impl DreamhackProvider { pub fn new(cfg: DreamhackConfig, store: Arc<dyn CredentialStore>) -> Result<Self>; }
impl WargameProvider for DreamhackProvider   // name() == "dreamhack"
pub const CREDENTIAL_KEY: &str = "dreamhack.session";
// model.rs: SearchResponse{count,next,previous,results:Vec<ChallengeListItem>}, ChallengeListItem{id:u64,title,tags:Vec<String>,tier_display:Option<String>,repository:Option<String>,needs_vm:bool,is_public:bool}, ChallengeDetail{ + description:String }, all with #[serde(default)] except id/title
// normalize.rs: pub fn summary(item:&ChallengeListItem)->ChallengeSummary (slug = repository.unwrap_or(slugify(title)); difficulty = tier_display; categories = tags); pub fn challenge(d:&ChallengeDetail)->Challenge (source_url = base/wargame/challenges/<id>; assets = vec![ChallengeAsset{id: id.to_string(), filename: format!("{slug}.zip"), descriptor: json!({"kind":"dreamhack-download","challenge_id":id}), ..}] when needs_vm || description contains "file"/"파일"/"binary"/"바이너리"; remote_endpoints = lines of description matching `nc <host> <port>` or `http(s)://…`)
// auth.rs: login(): GET base/ to obtain csrftoken cookie (ignore failures), POST /api/v1/auth/login/ json {email, password} with X-CSRFToken + Referer; on 2xx serialize cookie jar (name,value,domain,path) as JSON → store.set(CREDENTIAL_KEY); on 401 → AuthenticationFailed; password is `SecretString` and never logged
// download.rs: POST /api/v1/wargame/challenges/<id>/download/ with headers X-CSRFToken (from cookie "csrftoken"), Referer base/wargame/challenges/<id>/; 401/403 → AuthenticationRequired; 404 → ChallengeNotFound; if content-type JSON: find url in ["url","file","download_url"] or array "files"[i]["url"|"file"] (+ "name"/"filename") → GET each, stream into sink; else if body is a file (Content-Disposition present or content-type octet-stream/zip): stream response directly; else ProviderChanged{what:"download response shape"}. Filename from Content-Disposition (RFC 6266 `filename=` / `filename*=UTF-8''`) or from the URL path or the JSON name; passed to sink.begin() which sanitizes.
```

Cookie persistence: on `new()`, read `store.get(CREDENTIAL_KEY)`; if present, parse and add cookies to a `reqwest::cookie::Jar` for `base_url`. `auth_state()` → Authenticated if a `sessionid` cookie is stored (display = "session cookie"). `healthcheck()` → GET search with page_size 1; reachable if 2xx.

- [ ] **Step 1:** Fixture tests: `fixtures/search.json` parses to 1 item with id 11, title `out_of_bound`, slug `out-of-bound`, difficulty `Bronze 4`, categories `["pwnable"]`; `search_page.json` yields `next_page == Some(2)`; `detail.json` normalizes with description non-empty, one asset with descriptor kind `dreamhack-download`, `remote_endpoints` empty; `login_401.json`/`download_401.json` map to the right errors; `Content-Disposition` parser handles `attachment; filename="a.zip"` and `filename*=UTF-8''%ED%95%9C.zip`; CSRF header equals the cookie value.
- [ ] **Step 2:** red. **Step 3:** Implement (HTTP via `reqwest::Client` built with `cookie_provider(jar)`, `use_rustls_tls`, `timeout 30s`). No live network in tests. **Step 4:** green + clippy + fmt.

---

### Task 4: Tart provider crate

**Files:**
- Create: `crates/wargame-tart/src/{lib,args,list,image,provider,console}.rs`
- Tests: inline, all through `FakeRunner`.

**Interfaces (Consumes Task 1 `VmProvider`, Task 2 `ProcessRunner`/`FakeRunner`; Produces):**

```rust
pub struct TartConfig { pub binary: String /* "tart" */, pub vm_prefix: String /* "wg-" */, pub boot_timeout: Duration }
pub struct TartProvider { runner: Arc<dyn ProcessRunner>, cfg: TartConfig }
impl TartProvider { pub fn new(runner: Arc<dyn ProcessRunner>, cfg: TartConfig) -> Self; pub fn vm_name(&self, session_id: &str) -> String; }
impl VmProvider for TartProvider   // name() == "tart"; capabilities per DESIGN §5
// args.rs (pure, unit-tested):
pub fn clone_args(src:&str, dst:&str)->Vec<String>;                       // ["clone", src, dst]
pub fn set_args(vm:&str, r:&Resources)->Vec<String>;                       // ["set", vm, "--cpu", n, "--memory", mib, "--disk-size", gib]
pub fn run_args(vm:&str, o:&StartOptions)->Vec<String>;                    // ["run", vm, "--no-clipboard", "--no-audio", (+"--no-graphics" if headless), (+"--net-host" if Isolated)]
pub fn ip_args(vm:&str)->Vec<String>; pub fn stop_args; pub fn delete_args; pub fn list_args /* ["list","--format","json"] */; pub fn pull_args(image:&str);
// list.rs: #[derive(Deserialize)] pub struct TartVm { #[serde(rename="Name")] pub name:String, #[serde(rename="Source")] pub source:String, #[serde(rename="State")] pub state:String, #[serde(rename="Running")] pub running:bool, #[serde(rename="Size")] pub size_gib:u64, #[serde(rename="Disk")] pub disk_gib:u64 }
pub fn parse_list(json:&str)->Result<Vec<TartVm>>
// image.rs: pub fn is_oci(name:&str)->bool /* contains '/' or ':' */; image_exists = list contains Name == image (OCI names compare exactly, e.g. "ghcr.io/cirruslabs/macos-sequoia-vanilla:latest")
// provider.rs: create() = clone + set (on set failure: delete the clone, return VmCreationFailed); start() = spawn_detached(run_args, log_path) → RuntimeInfo{pid}; wait_ip() polls `tart ip <vm>` every 2s until it prints an IPv4 (parse) or boot_timeout → GuestBootTimeout; status() from list (Missing if absent); healthcheck() runs `tart --version`; delete() on a running VM: stop first (ignore error), then delete; list() returns VmRefs whose name starts with vm_prefix
// console.rs: console(vm) → ConsoleDescriptor::Window{ argv: [binary, "run", vm, "--no-clipboard", "--no-audio"] } (network flag appended by the caller via run_args)
```

- [ ] **Step 1:** Tests: every `*_args` exact vector; `parse_list` on the real sample from DESIGN §1 (3 entries, one running); `create()` issues clone then set and returns `VmRef{backend:"tart", name:"wg-<id>"}`; `create()` deletes on set failure; `wait_ip` returns `10.0.0.5` when the fake answers "10.0.0.5\n" after two failures; `start()` passes `--net-host` only for Isolated and `--no-graphics` only when headless; `status()` Missing for unknown VM; `delete()` stops a running VM first.
- [ ] **Step 2:** red. **Step 3:** implement. **Step 4:** green + clippy + fmt.

---

### Task 5: Application services (search, launch, lifecycle, console, doctor, profiles)

**Files:**
- Create: `crates/wargame-core/src/app/{mod,search,launch,lifecycle,console,doctor,profiles}.rs`
- Modify: `crates/wargame-core/src/lib.rs`
- Tests: `crates/wargame-core/tests/launch.rs`, `tests/lifecycle.rs` using Task 2 fakes.

**Interfaces (Consumes Tasks 1–2; Produces):**

```rust
pub struct AppContext { pub config: Config, pub dirs: AppDirs, pub wargame: Arc<dyn WargameProvider>, pub vm: Arc<dyn VmProvider>, pub transport: Arc<dyn GuestTransport>, pub repo: Arc<dyn SessionRepository>, pub profiles: ProfileStore, pub runner: Arc<dyn ProcessRunner>, pub bootstrap_script: &'static str }
pub struct ProfileStore { builtin: Vec<Profile>, user_dir: PathBuf }   // profiles.rs: load builtin (passed in by cli via include_str!) + *.yaml in user_dir (user overrides builtin by name); fn get(&self,name)->Result<Profile>; fn list(&self)->Vec<Profile>
pub struct LaunchRequest { pub challenge: ChallengeRef, pub profile: String, pub network: NetworkMode, pub resources: Option<Resources>, pub extract: bool, pub gui: bool }
pub enum LaunchEvent { Preflight(String), SessionCreated{id:String}, VmCreated{name:String}, Downloading{filename:String}, Downloaded{filename:String,sha256:String,size:u64}, Booting, GuestIp(IpAddr), Bootstrapping, Ready{session_id:String,ssh_hint:String}, Failed{session_id:Option<String>,error:String,hint:Option<String>} }
pub async fn launch(ctx:&AppContext, req:LaunchRequest, events: tokio::sync::mpsc::Sender<LaunchEvent>) -> Result<Session>   // DESIGN §3.4 exactly, including persisting after every state change and setting Failed + last_error on error
pub async fn search(ctx:&AppContext, q:SearchQuery)->Result<SearchPage>; pub async fn open(ctx:&AppContext, r:&ChallengeRef)->Result<Challenge>
// lifecycle.rs
pub async fn list_sessions(ctx)->Result<Vec<Session>>; pub async fn inspect(ctx,id)->Result<Session>
pub async fn stop(ctx,id)->Result<Session>        // Ready|Running → vm.stop → Stopped
pub async fn resume(ctx,id)->Result<Session>      // Stopped → Booting (vm.start headless with saved network) → wait_ip → transport.wait_ready → Running
pub async fn delete(ctx,id,force:bool)->Result<()> // Keep policy requires force → else Err(Other("session is kept; use --force")); Deleting → vm.delete (Missing is fine) → repo.delete
pub async fn keep(ctx,id,keep:bool)->Result<Session>
pub async fn prune(ctx, dry_run:bool)->Result<Vec<String>>   // vm.list() names with prefix not matching any session → delete unless dry_run
// console.rs
pub async fn ssh_command(ctx,id)->Result<std::process::Command>   // requires is_up and guest_ip
pub async fn open_gui(ctx,id)->Result<()>   // if running: vm.stop; then vm.start(StartOptions{headless:false,..}) → wait_ip → wait_ready → update guest_ip/pid; state Running
// doctor.rs
pub struct Check { pub name:String, pub ok:bool, pub detail:String, pub hint:Option<String> }
pub async fn doctor(ctx)->Vec<Check>   // DESIGN §7 list; each check independent; never returns Err
```

Guest layout written by launch: `<workspace.root>/.wg/bootstrap.sh`, `<workspace.root>/<slug>/incoming/<files>`, `<workspace.root>/<slug>/incoming/.wargame.json` (metadata per MVP §15). Bootstrap exec: `bash <root>/.wg/bootstrap.sh <root>/<slug> --extract=<true|false>`.

- [ ] **Step 1:** Tests with fakes: happy path emits events in order and ends `Ready`, session persisted with `assets[0].sha256` equal to sha256 of the fake bytes, transport `copy_in` called for the asset and bootstrap, `exec` called with `--extract=true`; staging dir removed afterwards; `ImageMissing` when fake `image_exists=false` and no session is created; VM create failure → session `Failed` with `last_error` and `vm_ref == None`; boot timeout → `Failed` with `vm_ref` set (so delete works); anonymous provider + assets → `AuthenticationRequired` before any VM; `delete` refuses `Keep` without force; `prune` finds `wg-orphan`; `compat` rejection produces no session.
- [ ] **Step 2:** red. **Step 3:** implement. **Step 4:** green + clippy + fmt.

---

### Task 6: Profile image builder, profile definitions, guest bootstrap

**Files:**
- Create: `crates/wargame-tart/src/build.rs`; Modify `crates/wargame-tart/src/lib.rs`
- Create: `profiles/macos-minimal-arm64.yaml`, `profiles/macos-re-arm64.yaml`, `profiles/scripts/macos/common.sh`, `profiles/scripts/macos/reversing.sh`
- Create: `guest/bootstrap.sh`
- Tests: `crates/wargame-tart/src/build.rs` inline (argv/order through `FakeRunner` + `FakeTransport`), `crates/wargame-core/tests/profiles.rs` (both YAML files parse), `guest/bootstrap_test.sh` (bash test runnable on the host with a temp dir; run by `cargo test -p wargame-cli` via `std::process::Command` only when `bash` and `unzip` exist).

**Interfaces (Consumes Tasks 1, 2, 4; Produces):**

```rust
pub struct BuildOptions { pub scripts_dir: PathBuf /* repo profiles/ dir or $WARGAME_HOME/profiles */, pub pubkey: String, pub log_path: PathBuf }
pub struct ProfileImageBuilder { runner: Arc<dyn ProcessRunner>, tart: Arc<TartProvider>, transport_factory: Box<dyn Fn(&str /*user*/) -> Arc<dyn GuestTransport> + Send + Sync> }
pub async fn build_profile(&self, profile:&Profile, opts:&BuildOptions, progress: &mut dyn FnMut(String)) -> Result<ImageRef>
// steps: 1 ensure base pulled (image_exists else pull); 2 if "wargame-profile-<name>" exists → delete it; 3 clone base → profile image name; 4 set resources; 5 start headless with Nat, log to opts.log_path; 6 wait_ip; 7 password bootstrap: run `sshpass -e ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null user@ip "mkdir -p ~/.ssh && chmod 700 ~/.ssh && printf '%s\n' '<pubkey>' >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys"` with env SSHPASS=<profile.guest.password> (env, never argv) retrying every 5s up to 10 min; 8 transport.wait_ready; 9 copy_in scripts dir → ~/wg-provision/; 10 exec each `bash ~/wg-provision/<script>`; 11 exec each tools.verify (fail → InvalidProfile{reason}); 12 exec `rm -rf ~/wg-provision`; 13 stop; return ImageRef(profile.image_name())
```

`common.sh`: idempotent; installs Homebrew non-interactively if missing (`NONINTERACTIVE=1`), writes `eval "$(/opt/homebrew/bin/brew shellenv)"` into `~/.zprofile`, `brew install git python uv curl wget jq vim tree`, disables screen lock/sleep (`sudo pmset -a sleep 0 displaysleep 0`), creates `~/wargame`. `reversing.sh`: `brew install --cask temurin`, `brew install ghidra llvm binutils hexyl file`, `brew install --cask ghidra` if formula absent.

`guest/bootstrap.sh <workspace-dir> --extract=<true|false>`: creates `incoming/ work/ notes/ output/`, moves nothing, when extract=true unpacks `incoming/*.zip` via `unzip -n -q` into `incoming/extracted/<name>/`, `*.tar.gz|*.tgz|*.tar` via `tar -xf … -C` (bsdtar refuses absolute/`..`), never runs anything else, writes `notes/README.md` with the metadata summary from `incoming/.wargame.json` (via `python3 -c` if python3 exists, else raw copy), prints the welcome block. Exit non-zero on any error (`set -euo pipefail`).

- [ ] **Step 1:** Tests: builder call order recorded on `FakeRunner` (`clone`, `set`, `run`, `ip`, sshpass with `SSHPASS` in env and not in args, scripts executed in profile order, `verify` commands executed, `stop` last); verify failure → `InvalidProfile`; both YAMLs parse and `macos-re-arm64.provision.scripts.len()==2`; `bootstrap_test.sh`: zip with `../evil.txt` entry is not written outside the workspace; directories exist; `--extract=false` leaves the zip untouched.
- [ ] **Step 2:** red. **Step 3:** implement. **Step 4:** green + clippy + fmt + `shellcheck`-clean if available.

---

### Task 7: CLI binary and composition root

**Files:**
- Create: `crates/wargame-cli/src/{main,cli,wiring,output,fixture_provider}.rs`; `crates/wargame-cli/src/profiles_builtin.rs` (`include_str!` of both YAMLs and `guest/bootstrap.sh`)
- Tests: `crates/wargame-cli/tests/cli.rs` (uses `std::process::Command` on `env!("CARGO_BIN_EXE_wargame")` with `WARGAME_HOME` = tempdir and `WARGAME_PROVIDER=fixture`, `WARGAME_VM=fake`).

**Interfaces (Consumes Tasks 1–6; Produces the `wargame` binary):**

```
wargame                                  → TUI (Task 8; until then prints "TUI not built yet" and exits 2)
wargame search <query> [--category c] [--page n] [--json]
wargame open <provider:id|id> [--json]
wargame launch <ref> [--profile P] [--network isolated|nat] [--cpu n] [--memory-gib n] [--no-extract] [--gui] [--yes]
wargame sessions [--json]
wargame session inspect <id> [--json]
wargame ssh <id>            (exec replaces process)
wargame gui <id>
wargame stop <id> | resume <id> | keep <id> [--off] | delete <id> [--force] | prune [--dry-run]
wargame doctor [--json]
wargame profiles [--json] | wargame profile build <name>
wargame auth login [--email e] | auth status | auth logout
global: --verbose, --config <path>, --home <path>
```

`wiring.rs`: `build_context(cli_globals) -> Result<AppContext>`: `AppDirs::resolve` (+`--home`), `ensure_layout`, copy builtin profile YAMLs into `profiles_dir` if absent, `Config::load`, credential store from config (`keychain` → `KeychainStore`, `file` → `FileStore`), provider from `WARGAME_PROVIDER` env (`fixture` → `FixtureProvider`, else Dreamhack), VM from `WARGAME_VM` env (`fake` → `FakeVmProvider` + `FakeTransport`, else `TartProvider` + `SshGuestTransport` with key `keys/id_ed25519`), `init_tracing(verbose, logs/wargame.log)`.

`fixture_provider.rs`: `FixtureProvider` implementing `WargameProvider` with one challenge `fixture:1` (`hello-fixture`) whose asset is an in-memory zip built at runtime with the `zip`-free approach: a stored (uncompressed) ZIP written by hand (local header + central directory + EOCD for a single file `hello.txt` containing `hello wargame\n`). This is what the E2E smoke test uses.

NAT confirmation (MVP §18): without `--yes`, `launch --network nat` prints the warning and asks `Continue? [y/N]` on the TTY. Errors: print `Error: <msg>` and `Try: <hint>` on stderr, exit 1. `launch` prints each `LaunchEvent` on its own line with `✓`/`…` markers.

- [ ] **Step 1:** CLI tests: `wargame --help` lists all commands; `wargame profiles --json` lists both builtin profiles; `wargame search hello --json` with fixture provider returns `fixture:1`; `wargame launch fixture:1 --profile macos-minimal-arm64 --yes` with fake VM prints `Ready` and `wargame sessions --json` then shows one session in state `ready` with `assets[0].sha256` set; `wargame delete <id>` removes it; `wargame doctor --json` returns an array and exits 0 even when tart is missing; `wargame launch fixture:1 --network nat` without `--yes` and without a TTY exits 1 with "Continue".
- [ ] **Step 2:** red. **Step 3:** implement. **Step 4:** green + clippy + fmt.

---

### Task 8: TUI

**Files:**
- Create: `crates/wargame-cli/src/tui/{mod,app,search,problem,launch,sessions,widgets}.rs`; Modify `main.rs` (no subcommand → `tui::run(ctx)`)
- Tests: `tui/app.rs` inline tests for the pure state reducer (`App::handle(Event) -> Vec<Effect>`); no terminal needed.

**Design:** Elm-style. `App { screen: Screen, search: SearchState, problem: Option<ProblemState>, launch: Option<LaunchState>, sessions: Vec<Session>, status: String }`. `Screen::{Search, Problem, Launching, Sessions}`. Input events (`crossterm::event::KeyEvent`) and async results (`Msg::{SearchResults(SearchPage), Challenge(Challenge), Launch(LaunchEvent), Sessions(Vec<Session>), Error(String)}`) go through one `update(&mut self, Msg) -> Vec<Effect>`; effects (`Effect::{Search(SearchQuery), Open(ChallengeRef), Launch(LaunchRequest), LoadSessions, Stop(id), Resume(id), Delete(id), ExecSsh(id), OpenGui(id), Quit}`) are executed by the runtime loop with `tokio::spawn` and results sent back through an `mpsc` channel. Key map per DESIGN §7 and MVP §6 mockups (`/` search, `Enter` open, `Tab` cycle profile/network fields, `n` toggles network with the NAT warning shown in red, `l` launch, `s` sessions screen, `Esc` back, `q` quit, on sessions: `x` stop, `r` resume, `d` delete (asks y/N), `Enter` ssh). `ExecSsh` suspends the TUI (leave alternate screen, run ssh, re-enter).

- [ ] **Step 1:** Reducer tests: typing then Enter emits `Effect::Search`; `SearchResults` fills the list and `Enter` emits `Open`; on Problem, `n` toggles Nat and status shows the warning; `l` emits `Launch` with the selected profile/network; `Launch(Ready)` sets screen Launching with ready flag; `q` emits Quit.
- [ ] **Step 2:** red. **Step 3:** implement, and run `wargame` once with `WARGAME_PROVIDER=fixture WARGAME_VM=fake` under `script -q /dev/null` to check it draws without panicking (exit with `q`). **Step 4:** green + clippy + fmt.

---

### Task 9: README, e2e smoke, nix wrapper, doctor polish

**Files:**
- Create: `README.md` (install via `nix run github:…`/`nix build`, first-run walkthrough per MVP §7.1, security notes, what "isolated" means with `--net-host`, troubleshooting), `scripts/e2e-smoke.sh`, `crates/wargame-tart/tests/e2e.rs` (`#[ignore]`, requires `WARGAME_E2E=1`), `CHANGELOG.md`
- Modify: `flake.nix` (wrap binary with `makeWrapper --prefix PATH : ${lib.makeBinPath [tart sshpass openssh]}`; `checks.{fmt,clippy,tests}`), `crates/wargame-core/src/app/doctor.rs` if any check is missing from DESIGN §7.

`scripts/e2e-smoke.sh`: `export WARGAME_HOME=$(mktemp -d) WARGAME_PROVIDER=fixture`; builds with `cargo build`; `wargame doctor`; `wargame launch fixture:1 --profile <profile from $1, default macos-minimal-arm64> --yes`; `wargame ssh <id>`-less verification via `wargame session inspect --json` → guest_ip, then `ssh -i $WARGAME_HOME/keys/id_ed25519 admin@ip 'cat ~/wargame/hello-fixture/incoming/extracted/hello-fixture/hello.txt'` must print `hello wargame`; `wargame delete <id> --force`. `tests/e2e.rs` does the M0 flow directly against the vanilla image with password auth (documented; only runs when `WARGAME_E2E=1`).

- [ ] Write files; `nix flake check`; `nix build`; `./result/bin/wargame --help`; `./result/bin/wargame doctor`.

---

## Execution workflow

Stages (each stage's tasks run in parallel; stages are sequential; each task = implement → review → fix-if-needed → re-review; a single commit agent commits after each stage):

| Stage | Tasks |
|---|---|
| A | 1 |
| B | 2, 3, 4 |
| C | 5, 6 |
| D | 7 |
| E | 8, 9 |

Reviewer gate: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` all green inside `nix develop`; task acceptance tests present and passing; global constraints honoured; no secret in argv; no host-side extraction. Verdict schema `{approved: bool, issues: [{severity, file, description}], summary}`.

After Stage E the orchestrator runs: `nix flake check`, `nix build`, `wargame doctor`, `scripts/e2e-smoke.sh` against the local vanilla image (M0 proof), and records results in `docs/verification.md`.
