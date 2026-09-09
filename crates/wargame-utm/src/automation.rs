//! Every external invocation the UTM backend makes: `utmctl`, `osascript`,
//! `open -a UTM` and the one `ps` of trap T12. Nothing else in this crate
//! runs a process.
//!
//! Two rules come straight from `docs/utm-compatibility.md` and spec.md §5
//! item 6:
//!
//! * **AppleScript templates are trusted constants.** Every value that
//!   comes from a profile, a session or the user -- names, UUIDs, paths,
//!   sizes -- is passed as an `osascript` argument and read back inside the
//!   script through `on run argv`. The script text never contains user
//!   data, so there is nothing to escape and no host shell string is ever
//!   built (everything goes through [`ProcessRunner`] argv).
//! * **`utmctl`'s exit code does not report Apple-Event failures** (T1). A
//!   command counts as failed when stderr carries `Error from event:` or
//!   `Error:`, whatever the exit code says, and errors are matched on their
//!   numeric code or an ASCII-only prefix -- never on prose, which UTM
//!   writes with typographic apostrophes.

use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::time::{sleep, Instant};

use wargame_core::error::{Result, WargameError};
use wargame_core::infra::process::{Cmd, Output, ProcessRunner};

use crate::config::{parse_version, UtmConfig, BUNDLED_UTMCTL, UTM_APP};

/// The sentinel an id-addressed script returns when no VM carries that
/// UUID. ASCII-only and unambiguous, so it can never collide with a status
/// value or a localized message.
pub const VM_NOT_FOUND: &str = "wargame-utm:vm-not-found";

/// The `status` values UTM's sdef defines (docs/utm-compatibility.md §2's
/// state machine). This is the complete set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UtmStatus {
    Stopped,
    Starting,
    Started,
    Pausing,
    Paused,
    Resuming,
    Stopping,
    /// Not registered in UTM at all, or registered with an `unavailable`
    /// backend -- both mean "do not act on this VM".
    Missing,
}

impl UtmStatus {
    pub fn parse(raw: &str) -> Option<UtmStatus> {
        Some(match raw.trim() {
            "stopped" => UtmStatus::Stopped,
            "starting" => UtmStatus::Starting,
            "started" => UtmStatus::Started,
            "pausing" => UtmStatus::Pausing,
            "paused" => UtmStatus::Paused,
            "resuming" => UtmStatus::Resuming,
            "stopping" => UtmStatus::Stopping,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            UtmStatus::Stopped => "stopped",
            UtmStatus::Starting => "starting",
            UtmStatus::Started => "started",
            UtmStatus::Pausing => "pausing",
            UtmStatus::Paused => "paused",
            UtmStatus::Resuming => "resuming",
            UtmStatus::Stopping => "stopping",
            UtmStatus::Missing => "missing",
        }
    }

    /// Whether the VM currently has a live backend. `stopping` counts as
    /// *not* live: T9 says to poll for `stopped`, never for "not started".
    pub fn is_live(self) -> bool {
        matches!(
            self,
            UtmStatus::Starting
                | UtmStatus::Started
                | UtmStatus::Pausing
                | UtmStatus::Paused
                | UtmStatus::Resuming
        )
    }
}

impl std::fmt::Display for UtmStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How to stop a VM (the sdef's `stop method`; `request` needs a guest OS
/// that listens and is not used here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMethod {
    /// ACPI power-off, `utmctl stop`'s default. Can sit in `stopping` (T9).
    Force,
    /// Terminate the backend. Fast, but `stopped` still does not mean the
    /// QEMU process is gone (T12).
    Kill,
}

impl StopMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            StopMethod::Force => "force",
            StopMethod::Kill => "kill",
        }
    }
}

/// One row of `utmctl list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UtmVm {
    pub id: String,
    pub status: UtmStatus,
    pub name: String,
}

/// What [`Automation::eject_removable_drives`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ejected {
    /// This many removable drives were detached; `0` means there were none.
    Detached(usize),
    /// **Nothing was written.** Every drive the VM has is removable, so
    /// writing the list back would have left it with no drives at all --
    /// which, on a VM adopted with `--from-vm`, is the user's installed
    /// system.
    EveryDriveRemovable,
}

/// A `qemu-*-softmmu` process still carrying a VM's `-uuid` after it was
/// stopped (T12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanQemu {
    pub pid: u32,
    /// The QEMU binary from the process's argv -- enough to identify it
    /// without dumping ~2.9 KB of command line.
    pub program: String,
}

/// Where `utmctl` was found (FR-6's discovery order), for `doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UtmctlSource {
    Config,
    Path,
    Bundle,
}

impl UtmctlSource {
    pub fn as_str(self) -> &'static str {
        match self {
            UtmctlSource::Config => "[vm.utm].utmctl",
            UtmctlSource::Path => "PATH",
            UtmctlSource::Bundle => "UTM.app bundle",
        }
    }
}

/// Parameters for `make new virtual machine`. Every field is written into
/// the configuration UTM creates; nothing is read back afterwards, because
/// `source` and `guest size` are write-only in practice (T5).
#[derive(Debug, Clone)]
pub struct MakeVmParams<'a> {
    pub name: &'a str,
    /// `"aarch64"` or `"x86_64"`.
    pub architecture: &'a str,
    pub memory_mib: u64,
    pub cpu_cores: u32,
    /// Must be `false` whenever the guest architecture differs from the
    /// host's: UTM accepts `true` and ignores it (T6).
    pub hypervisor: bool,
    pub uefi: bool,
    pub disk_mib: u64,
    /// Installer ISO for the first (removable) drive.
    pub iso: Option<&'a Path>,
}

// --- AppleScript templates -------------------------------------------------
//
// Each `*_BODY` runs inside `tell application "UTM"` with `vm` bound to the
// VM whose id is `item 1 of argv`; each `*_PROLOGUE` binds the remaining
// arguments *before* that block, which is where `POSIX file` has to be
// evaluated (T5: inside the block UTM resolves the path in its own sandbox
// and fails with -1728).

const STATUS_BODY: &str = r#"return (status of vm as text) & tab & (backend of vm as text)"#;

const START_BODY: &str = r#"start vm
				return "ok""#;

const STOP_PROLOGUE: &str = r#"set stopMethod to item 2 of argv"#;
const STOP_BODY: &str = r#"if stopMethod is "kill" then
					stop vm by kill
				else
					stop vm by force
				end if
				return "ok""#;

const DELETE_BODY: &str = r#"delete vm
				return "ok""#;

const DUPLICATE_PROLOGUE: &str = r#"set newName to item 2 of argv"#;
// `duplicate` returns nothing at all (T2), so the caller finds the new UUID
// by comparing the registry before and after.
//
// Unlike every other verb here, `duplicate` cannot take the loop's own
// reference: `duplicate vm` (and `duplicate (contents of vm)`) fails with
// `Handler only handles single objects. (-10014)`, verified on UTM 4.7.5.
// It needs a single-object specifier, so the loop stays as the existence
// check -- which is what produces the not-found sentinel -- and the copy
// itself addresses the VM by `whose id is`, still never by name (T3).
const DUPLICATE_BODY: &str = r#"duplicate (first virtual machine whose id is vmId) with properties {configuration:{name:newName}}
				return "ok""#;

const HARDWARE_PROLOGUE: &str = r#"set memMib to (item 2 of argv) as integer
	set cores to (item 3 of argv) as integer
	set hyp to ((item 4 of argv) is "true")"#;
// `hypervisor` is written explicitly rather than inherited from whatever
// the source VM was built with: the session's execution mode is the one
// the profile resolved to. UTM will silently ignore `true` on a
// mismatched architecture (T6), which is why the caller refuses that
// combination before ever getting here.
const HARDWARE_BODY: &str = r#"set c to configuration of vm
				set memory of c to memMib
				set cpu cores of c to cores
				set hypervisor of c to hyp
				update configuration of vm with c
				return "ok""#;

const NETWORK_PROLOGUE: &str = r#"set netMode to item 2 of argv"#;
// Only `host` (our `isolated`) and `shared` (our `nat`) are reachable:
// `emulated` is libslirp, which is not isolated, and `bridged` puts the
// guest on the LAN (§5).
const NETWORK_BODY: &str = r#"set c to configuration of vm
				set n to index of item 1 of network interfaces of c
				if netMode is "host" then
					set item 1 of network interfaces of c to {index:n, mode:host}
				else
					set item 1 of network interfaces of c to {index:n, mode:shared}
				end if
				update configuration of vm with c
				return "ok""#;

const DISPLAY_PROLOGUE: &str = r#"set hw to item 2 of argv"#;
// A VM made by `make` has no display at all, and then the only console is
// the serial ptty (§3). An unknown hardware name is silently replaced by
// the default (T11), so the caller validates the name.
const DISPLAY_BODY: &str = r#"set c to configuration of vm
				set displays of c to {{hardware:hw}}
				update configuration of vm with c
				return "ok""#;

const ATTACH_ISO_PROLOGUE: &str = r#"set isoFile to POSIX file (item 2 of argv)"#;
const ATTACH_ISO_BODY: &str = r#"set c to configuration of vm
				set drives of c to (drives of c) & {{removable:true, source:isoFile}}
				update configuration of vm with c
				return "ok""#;

// Replacing an ISO must echo the drive's own id back, or UTM adds a drive
// instead of replacing one (§2).
const REPLACE_ISO_PROLOGUE: &str = r#"set isoFile to POSIX file (item 2 of argv)"#;
const REPLACE_ISO_BODY: &str = r#"set c to configuration of vm
				set i to id of item 1 of drives of c
				set item 1 of drives of c to {id:i, source:isoFile}
				update configuration of vm with c
				return "ok""#;

/// What the eject script answers when writing the drive list back would
/// leave the VM with **no drives at all** -- every drive it has reports
/// `removable: true`. Nothing is written in that case.
pub const NO_SURVIVING_DRIVE: &str = "wargame-utm:no-surviving-drive";

// Detaching the installer CD. UTM's dictionary has no "remove a drive"
// verb, so the drive *list* is written back without the removable entries
// -- the same `update configuration of drives` mechanism §2 verified for
// appending and replacing one, and the sequence the first real install had
// to run by hand (task-7-phase-a-report.md §3).
//
// Three rules, two from the readback and one from what this write could
// destroy:
//
// * a surviving drive must echo its own `id` back (`{id:(id of d)}`), or
//   UTM treats the record as a *new* drive and adds one;
// * `update configuration` needs a `stopped` VM (T9), which the caller
//   checks -- so the write only happens when something is actually removable
//   and the whole thing is a no-op otherwise;
// * **at least one drive must survive.** `removable` is a property of the
//   drive, set when it was created and not changeable afterwards, and a VM
//   adopted with `profile create --from-vm` can perfectly well have its
//   boot disk flagged removable. Writing `{}` back would detach the user's
//   installed system, irreversibly and silently. So the write is guarded by
//   a counter of the drives that would remain, and a VM where nothing would
//   remain is refused with [`NO_SURVIVING_DRIVE`] rather than written to.
//
// `kept`, `gone` and `keptCount` are deliberately ordinary names: a local
// whose name collides with an AppleScript parameter name fails to compile
// with `-2741 Expected expression but found parameter name` (D6). The
// survivors are counted in the loop rather than with `count of`, which UTM
// answers 0 for (T5) -- `kept` is a plain list and would count correctly,
// but counting as we go needs no such distinction.
const EJECT_MEDIA_BODY: &str = r#"set c to configuration of vm
				set kept to {}
				set gone to 0
				set keptCount to 0
				repeat with d in (drives of c)
					if (removable of d) is false then
						set end of kept to {id:(id of d)}
						set keptCount to keptCount + 1
					else
						set gone to gone + 1
					end if
				end repeat
				if gone > 0 then
					if keptCount is 0 then
						return "wargame-utm:no-surviving-drive"
					end if
					set drives of c to kept
					update configuration of vm with c
				end if
				return (gone as text)"#;

/// `make` is the only way to create a VM (`utmctl` has no create verb), and
/// unlike `duplicate` it does return a specifier, so the new UUID is read
/// straight back. `drives` is always explicit: omitting it still produces
/// two drives, one of them a CD with no image (T5).
const MAKE_WITH_ISO: &str = r#"on run argv
	set vmName to item 1 of argv
	set archName to item 2 of argv
	set memMib to (item 3 of argv) as integer
	set cores to (item 4 of argv) as integer
	set hyp to ((item 5 of argv) is "true")
	set useUefi to ((item 6 of argv) is "true")
	set diskMib to (item 7 of argv) as integer
	set isoFile to POSIX file (item 8 of argv)
	tell application "UTM"
		set vm to make new virtual machine with properties {backend:qemu, configuration:{name:vmName, architecture:archName, memory:memMib, cpu cores:cores, hypervisor:hyp, uefi:useUefi, drives:{{removable:true, source:isoFile}, {guest size:diskMib}}}}
		return (id of vm) & tab & (name of vm)
	end tell
end run
"#;

const MAKE_WITHOUT_ISO: &str = r#"on run argv
	set vmName to item 1 of argv
	set archName to item 2 of argv
	set memMib to (item 3 of argv) as integer
	set cores to (item 4 of argv) as integer
	set hyp to ((item 5 of argv) is "true")
	set useUefi to ((item 6 of argv) is "true")
	set diskMib to (item 7 of argv) as integer
	tell application "UTM"
		set vm to make new virtual machine with properties {backend:qemu, configuration:{name:vmName, architecture:archName, memory:memMib, cpu cores:cores, hypervisor:hyp, uefi:useUefi, drives:{{guest size:diskMib}}}}
		return (id of vm) & tab & (name of vm)
	end tell
end run
"#;

/// The automation-permission probe: the cheapest possible Apple Event to
/// UTM. A denied grant answers with -1743 (§1).
const APP_VERSION_SCRIPT: &str = r#"tell application "UTM" to return UTM version
"#;

/// Builds an id-addressed script: look the VM up by UUID, never by name
/// (T3: display names are not unique and `virtual machine named` silently
/// returns the first match).
fn vm_script(prologue: &str, body: &str) -> String {
    let prologue = if prologue.is_empty() {
        String::new()
    } else {
        format!("\t{prologue}\n")
    };
    format!(
        "on run argv\n\tset vmId to item 1 of argv\n{prologue}\ttell application \"UTM\"\n\t\trepeat with vm in (virtual machines)\n\t\t\tif (id of vm) is vmId then\n\t\t\t\t{body}\n\t\t\tend if\n\t\tend repeat\n\tend tell\n\treturn \"{VM_NOT_FOUND}\"\nend run\n"
    )
}

/// Whether `stderr` reports an Apple-Event failure even though the process
/// exited 0 (T1). Matches only ASCII-stable prefixes.
fn has_apple_event_error(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|line| line.starts_with("Error from event:") || line.starts_with("Error:"))
}

/// `utmctl`'s only non-zero exit: the VM is not registered (T1).
fn is_vm_not_found(stderr: &str) -> bool {
    stderr.contains("Virtual machine not found")
}

/// Whether `stderr` carries the OSStatus/AppleScript error `code` as a
/// *number* rather than as a substring: `-609` must not be found inside
/// `-6091`, and a path or a UUID that happens to contain those digits is
/// not an error code (§1 says to match on the number).
fn has_error_code(stderr: &str, code: &str) -> bool {
    let bytes = stderr.as_bytes();
    let mut from = 0;
    while let Some(offset) = stderr[from..].find(code) {
        let start = from + offset;
        let end = start + code.len();
        // A real code stands alone: UTM writes `error -609.)` or `(-1712)`,
        // so the characters around it are punctuation or whitespace, never
        // part of a longer number, identifier or path segment.
        let boundary = |byte: Option<&u8>| match byte {
            None => true,
            Some(b) => !(b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_'),
        };
        if boundary(start.checked_sub(1).map(|i| &bytes[i])) && boundary(bytes.get(end)) {
            return true;
        }
        from = end;
    }
    false
}

/// A denied macOS Automation grant, by error number only -- the prose
/// carries typographic apostrophes and is not matchable (§1).
fn is_automation_denied(stderr: &str) -> bool {
    has_error_code(stderr, "-1743") || stderr.contains("errAEEventNotPermitted")
}

/// The app was not up (`-609`) or was wedged (`-1712`) -- the two shapes
/// T8 says to retry once after re-handshaking.
fn is_transient_app_error(stderr: &str) -> bool {
    has_error_code(stderr, "-609") || has_error_code(stderr, "-1712")
}

/// The single line of `stderr` worth showing, or the whole thing when it is
/// short. Keeps `Error from event:`'s follow-up line, which is where UTM
/// puts the actual reason.
fn error_detail(stderr: &str) -> String {
    let text = stderr.trim();
    if text.is_empty() {
        return "no error output".to_string();
    }
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Parses `utmctl list`'s three-column table. The name is everything after
/// the status column, so display names with spaces (and Korean) survive.
pub fn parse_list(stdout: &str) -> Result<Vec<UtmVm>> {
    let changed = |what: String| WargameError::ProviderChanged {
        provider: "utm".to_string(),
        what,
    };
    let mut vms = Vec::new();
    for (index, line) in stdout.lines().enumerate() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if index == 0 {
            if !line.starts_with("UUID") {
                return Err(changed(format!(
                    "`utmctl list` header was {line:?}, expected a UUID/Status/Name table"
                )));
            }
            continue;
        }
        let (id, rest) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| changed(format!("`utmctl list` row {line:?} has no status column")))?;
        // A UUID is 36 characters; anything else means the table changed.
        if id.len() != 36 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Err(changed(format!(
                "`utmctl list` row {line:?} does not start with a UUID"
            )));
        }
        let rest = rest.trim_start();
        let (status, name) = rest
            .split_once(char::is_whitespace)
            .ok_or_else(|| changed(format!("`utmctl list` row {line:?} has no name column")))?;
        let status = UtmStatus::parse(status).ok_or_else(|| {
            changed(format!(
                "`utmctl list` reported status {status:?}, which is not in UTM's status enumeration"
            ))
        })?;
        vms.push(UtmVm {
            id: id.to_string(),
            status,
            name: name.trim().to_string(),
        });
    }
    Ok(vms)
}

/// The `ProcessRunner`-backed UTM automation surface.
pub struct Automation {
    runner: Arc<dyn ProcessRunner>,
    cfg: UtmConfig,
    /// The resolved `utmctl` path and where it came from, cached after the
    /// first successful discovery.
    utmctl: Mutex<Option<(String, UtmctlSource)>>,
    /// Whether the app handshake of T8 already succeeded in this process.
    app_ready: AtomicBool,
}

impl Automation {
    pub fn new(runner: Arc<dyn ProcessRunner>, cfg: UtmConfig) -> Self {
        Automation {
            runner,
            cfg,
            utmctl: Mutex::new(None),
            app_ready: AtomicBool::new(false),
        }
    }

    pub fn config(&self) -> &UtmConfig {
        &self.cfg
    }

    /// FR-6's discovery order: `[vm.utm].utmctl` -> `PATH` ->
    /// [`BUNDLED_UTMCTL`]. Paths with spaces are fine: they are argv
    /// elements, never shell words.
    pub fn utmctl_path(&self) -> Result<String> {
        Ok(self.utmctl_located()?.0)
    }

    /// [`Automation::utmctl_path`] plus where it was found, for `doctor`.
    pub fn utmctl_located(&self) -> Result<(String, UtmctlSource)> {
        if let Some(found) = self.utmctl.lock().unwrap().clone() {
            return Ok(found);
        }
        let candidates: Vec<(String, UtmctlSource)> = self
            .cfg
            .utmctl
            .iter()
            .map(|p| (p.clone(), UtmctlSource::Config))
            .chain(std::iter::once(("utmctl".to_string(), UtmctlSource::Path)))
            .chain(std::iter::once((
                BUNDLED_UTMCTL.to_string(),
                UtmctlSource::Bundle,
            )))
            .collect();
        for (candidate, source) in candidates {
            if let Some(path) = self.runner.which(&candidate) {
                let found = (path.display().to_string(), source);
                *self.utmctl.lock().unwrap() = Some(found.clone());
                return Ok(found);
            }
        }
        Err(WargameError::BackendUnavailable {
            backend: "utm".to_string(),
            reason: format!(
                "utmctl not found: not at {}, not on PATH, and not at {BUNDLED_UTMCTL}. Install UTM (https://mac.getutm.app or `brew install --cask utm`), or set [vm.utm].utmctl to its path",
                self.cfg
                    .utmctl
                    .as_deref()
                    .unwrap_or("(no [vm.utm].utmctl configured)")
            ),
        })
    }

    /// T8: `utmctl` and `osascript` fail (with exit 0) when the app is not
    /// up -- `-609` while it is launching, `-1712` when it is wedged. Bring
    /// it up and handshake with `utmctl version` before anything else.
    /// Cached: at most one handshake per process.
    pub async fn ensure_app_running(&self) -> Result<()> {
        if self.app_ready.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Resolving first means a host without UTM never gets an `open`.
        let utmctl = self.utmctl_path()?;
        let _ = self
            .runner
            .run(&Cmd::new(
                "open",
                vec!["-a".to_string(), UTM_APP.to_string()],
            ))
            .await;

        let deadline = Instant::now() + self.cfg.app_launch_timeout;
        loop {
            let out = self
                .runner
                .run(&Cmd::new(&utmctl, vec!["version".to_string()]))
                .await?;
            if out.status == 0 && !has_apple_event_error(&out.stderr) {
                if let Some(_version) = parse_version(&out.stdout) {
                    self.app_ready.store(true, Ordering::Relaxed);
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(WargameError::BackendUnavailable {
                    backend: "utm".to_string(),
                    reason: format!(
                        "UTM did not answer `utmctl version` within {:?}: {}",
                        self.cfg.app_launch_timeout,
                        error_detail(&out.stderr)
                    ),
                });
            }
            sleep(self.cfg.status_poll_interval).await;
        }
    }

    /// `utmctl version`, without the handshake (this *is* the handshake's
    /// probe). Returns the raw version string.
    pub async fn version(&self) -> Result<String> {
        let utmctl = self.utmctl_path()?;
        let out = self
            .runner
            .run(&Cmd::new(&utmctl, vec!["version".to_string()]))
            .await?;
        if out.status != 0 || has_apple_event_error(&out.stderr) {
            return Err(self.classify("utmctl version", &out));
        }
        Ok(out.stdout.trim().to_string())
    }

    /// The automation-permission probe: an Apple Event to UTM from an
    /// out-of-process client. `-1743` means the grant is missing (§1).
    pub async fn automation_probe(&self) -> Result<String> {
        let out = self.osascript_raw(APP_VERSION_SCRIPT, &[]).await?;
        if out.status != 0 || has_apple_event_error(&out.stderr) {
            return Err(self.classify("osascript (UTM version)", &out));
        }
        Ok(out.stdout.trim().to_string())
    }

    /// Every registered VM.
    pub async fn list(&self) -> Result<Vec<UtmVm>> {
        self.ensure_app_running().await?;
        let out = self.utmctl(&["list"]).await?;
        parse_list(&out)
    }

    /// The VM with this UUID, or `None` when nothing is registered under it.
    pub async fn find(&self, id: &str) -> Result<Option<UtmVm>> {
        Ok(self.list().await?.into_iter().find(|vm| vm.id == id))
    }

    /// A VM's status, by UUID. A VM that is gone -- or whose backend reports
    /// `unavailable` -- is [`UtmStatus::Missing`], never an error: FR-5
    /// requires that to reconcile a session, not to create anything.
    pub async fn status(&self, id: &str) -> Result<UtmStatus> {
        self.ensure_app_running().await?;
        let out = self
            .osascript(&vm_script("", STATUS_BODY), &[id])
            .await
            .or_else(|e| match e {
                // `utmctl`'s own not-found shape reaches us the same way.
                WargameError::VmNotFound(_) => Ok(VM_NOT_FOUND.to_string()),
                other => Err(other),
            })?;
        let out = out.trim();
        if out == VM_NOT_FOUND || out.is_empty() {
            return Ok(UtmStatus::Missing);
        }
        let (status, backend) = out.split_once('\t').unwrap_or((out, "qemu"));
        if backend.trim() == "unavailable" {
            tracing::warn!(
                vm = %id,
                "UTM reports this VM's backend as unavailable; treating it as missing"
            );
            return Ok(UtmStatus::Missing);
        }
        UtmStatus::parse(status).ok_or_else(|| WargameError::ProviderChanged {
            provider: "utm".to_string(),
            what: format!(
                "`status of vm` returned {status:?}, which is not in UTM's status enumeration"
            ),
        })
    }

    /// Runs an id-addressed script that acts on the VM and returns `"ok"`,
    /// after making sure the app is up (T8).
    ///
    /// **The sentinel is an error here.** Every id-addressed script falls
    /// through its `repeat` loop when no VM carries that UUID and returns
    /// [`VM_NOT_FOUND`] with exit 0 -- a perfectly successful `osascript`
    /// run. Discarding that would turn "the VM is gone" into a silent
    /// no-op reported as success: a `set_network_mode` that never ran while
    /// an isolated session boots on whatever the source VM had, a delete
    /// that deleted nothing. Only [`Automation::status`] interprets the
    /// sentinel itself, because there "missing" is the answer rather than a
    /// failure.
    async fn run_on_vm(&self, id: &str, script: &str, args: &[&str]) -> Result<()> {
        self.run_on_vm_for(id, script, args).await.map(|_| ())
    }

    /// [`Automation::run_on_vm`] for the scripts whose *answer* matters
    /// (the eject reports how many drives it removed).
    async fn run_on_vm_for(&self, id: &str, script: &str, args: &[&str]) -> Result<String> {
        self.ensure_app_running().await?;
        let mut argv = vec![id];
        argv.extend_from_slice(args);
        let out = self.osascript(script, &argv).await?;
        if out.trim() == VM_NOT_FOUND {
            return Err(WargameError::VmNotFound(format!(
                "UTM has no virtual machine with id {id}"
            )));
        }
        Ok(out)
    }

    pub async fn start(&self, id: &str) -> Result<()> {
        self.run_on_vm(id, &vm_script("", START_BODY), &[]).await
    }

    pub async fn stop(&self, id: &str, method: StopMethod) -> Result<()> {
        self.run_on_vm(id, &vm_script(STOP_PROLOGUE, STOP_BODY), &[method.as_str()])
            .await
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        self.run_on_vm(id, &vm_script("", DELETE_BODY), &[]).await
    }

    /// `duplicate`, which returns nothing (T2). The caller resolves the new
    /// UUID by comparing the registry before and after.
    pub async fn duplicate(&self, source_id: &str, new_name: &str) -> Result<()> {
        self.run_on_vm(
            source_id,
            &vm_script(DUPLICATE_PROLOGUE, DUPLICATE_BODY),
            &[new_name],
        )
        .await
    }

    /// `make new virtual machine`, returning `(id, name)` read back from
    /// the specifier it produces.
    pub async fn make_vm(&self, params: &MakeVmParams<'_>) -> Result<(String, String)> {
        self.ensure_app_running().await?;
        let mut args = vec![
            params.name.to_string(),
            params.architecture.to_string(),
            params.memory_mib.to_string(),
            params.cpu_cores.to_string(),
            params.hypervisor.to_string(),
            params.uefi.to_string(),
            params.disk_mib.to_string(),
        ];
        let script = match params.iso {
            Some(iso) => {
                args.push(iso.display().to_string());
                MAKE_WITH_ISO
            }
            None => MAKE_WITHOUT_ISO,
        };
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = self.osascript(script, &refs).await?;
        let (id, name) =
            out.trim()
                .split_once('\t')
                .ok_or_else(|| WargameError::ProviderChanged {
                    provider: "utm".to_string(),
                    what: format!(
                        "`make new virtual machine` returned {out:?}, expected \"<id>\\t<name>\""
                    ),
                })?;
        Ok((id.trim().to_string(), name.trim().to_string()))
    }

    /// CPU, memory and the hypervisor flag. The VM must be `stopped`, else
    /// -2700 (T9).
    pub async fn set_hardware(
        &self,
        id: &str,
        cpu_cores: u32,
        memory_mib: u64,
        hypervisor: bool,
    ) -> Result<()> {
        self.run_on_vm(
            id,
            &vm_script(HARDWARE_PROLOGUE, HARDWARE_BODY),
            &[
                &memory_mib.to_string(),
                &cpu_cores.to_string(),
                &hypervisor.to_string(),
            ],
        )
        .await
    }

    /// `host` (isolated) or `shared` (NAT) on the first interface.
    pub async fn set_network_mode(&self, id: &str, mode: &str) -> Result<()> {
        self.run_on_vm(id, &vm_script(NETWORK_PROLOGUE, NETWORK_BODY), &[mode])
            .await
    }

    /// Gives the VM exactly one display, so UTM opens a window for it (§3).
    pub async fn set_display(&self, id: &str, hardware: &str) -> Result<()> {
        self.run_on_vm(id, &vm_script(DISPLAY_PROLOGUE, DISPLAY_BODY), &[hardware])
            .await
    }

    /// Appends a removable drive backed by `iso` (the Windows guest-tools
    /// CD case, §2).
    pub async fn attach_iso(&self, id: &str, iso: &Path) -> Result<()> {
        self.run_on_vm(
            id,
            &vm_script(ATTACH_ISO_PROLOGUE, ATTACH_ISO_BODY),
            &[&iso.display().to_string()],
        )
        .await
    }

    /// Points the first removable drive at another ISO.
    pub async fn replace_iso(&self, id: &str, iso: &Path) -> Result<()> {
        self.run_on_vm(
            id,
            &vm_script(REPLACE_ISO_PROLOGUE, REPLACE_ISO_BODY),
            &[&iso.display().to_string()],
        )
        .await
    }

    /// Detaches every removable drive (the installer CD) by writing the
    /// drive list back without them.
    ///
    /// [`Ejected::Detached(0)`](Ejected::Detached) is a success: an eject is
    /// idempotent. [`Ejected::EveryDriveRemovable`] means the write was
    /// refused because nothing would have survived it -- see
    /// [`NO_SURVIVING_DRIVE`].
    ///
    /// The VM must be `stopped` (T9); the caller checks that, because the
    /// answer to "it is running" is a recovery step, not a retry.
    pub async fn eject_removable_drives(&self, id: &str) -> Result<Ejected> {
        let out = self
            .run_on_vm_for(id, &vm_script("", EJECT_MEDIA_BODY), &[])
            .await?;
        let answer = out.trim();
        if answer == NO_SURVIVING_DRIVE {
            return Ok(Ejected::EveryDriveRemovable);
        }
        answer
            .parse::<usize>()
            .map(Ejected::Detached)
            .map_err(|_| WargameError::ProviderChanged {
                provider: "utm".to_string(),
                what: format!(
                    "detaching the removable drives of {id} answered {answer:?}, expected the number of drives it removed"
                ),
            })
    }

    /// The guest's address, via the QEMU guest agent. `Ok(None)` means the
    /// agent has not answered yet -- the normal state before provisioning
    /// installs it -- and carries the reason UTM gave.
    pub async fn guest_ip(&self, id: &str) -> Result<std::result::Result<IpAddr, String>> {
        self.ensure_app_running().await?;
        let utmctl = self.utmctl_path()?;
        let out = self
            .runner
            .run(&Cmd::new(
                &utmctl,
                vec!["ip-address".to_string(), id.to_string()],
            ))
            .await?;
        if out.status != 0 || has_apple_event_error(&out.stderr) {
            if is_automation_denied(&out.stderr) {
                return Err(self.classify("utmctl ip-address", &out));
            }
            return Ok(Err(error_detail(&out.stderr)));
        }
        // IPv4 first, then anything else routable (§5's guest address
        // discovery); link-local and loopback are of no use to ssh.
        let addresses: Vec<IpAddr> = out
            .stdout
            .lines()
            .filter_map(|line| IpAddr::from_str(line.trim()).ok())
            .filter(|ip| !ip.is_loopback() && !is_link_local(ip))
            .collect();
        match addresses
            .iter()
            .find(|ip| ip.is_ipv4())
            .or_else(|| addresses.first())
        {
            Some(ip) => Ok(Ok(*ip)),
            None => Ok(Err(format!(
                "the guest agent reported no usable address ({})",
                out.stdout.trim().replace('\n', ", ")
            ))),
        }
    }

    /// T12: a `qemu-*-softmmu` process whose argv `-uuid` is this VM's id,
    /// still alive after UTM reported `stopped`. QEMU's `-uuid` equals UTM's
    /// VM id exactly, so this needs no name matching.
    pub async fn find_orphan_qemu(&self, id: &str) -> Result<Option<OrphanQemu>> {
        let out = self
            .runner
            .run(&Cmd::new(
                "ps",
                vec![
                    "-Aww".to_string(),
                    "-o".to_string(),
                    "pid=,command=".to_string(),
                ],
            ))
            .await?;
        if out.status != 0 {
            // A `ps` we cannot run is not evidence of an orphan; say so and
            // let the stop succeed rather than blocking on a broken probe.
            tracing::warn!(
                status = out.status,
                "ps failed; skipping the orphan-QEMU check"
            );
            return Ok(None);
        }
        let needle = format!("-UUID {}", id.to_ascii_uppercase());
        for line in out.stdout.lines() {
            let line = line.trim_start();
            let Some((pid, command)) = line.split_once(char::is_whitespace) else {
                continue;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                continue;
            };
            if !command.contains("-softmmu") {
                continue;
            }
            if !command.to_ascii_uppercase().contains(&needle) {
                continue;
            }
            let program = command
                .split_whitespace()
                .find(|token| token.contains("-softmmu"))
                .unwrap_or("qemu")
                .to_string();
            return Ok(Some(OrphanQemu { pid, program }));
        }
        Ok(None)
    }

    /// Brings the UTM app to the front (the other half of "open the
    /// console" -- the window itself exists only if the VM has a display).
    pub async fn activate_app(&self) -> Result<()> {
        self.runner
            .run(&Cmd::new(
                "open",
                vec!["-a".to_string(), UTM_APP.to_string()],
            ))
            .await?;
        Ok(())
    }

    // --- process plumbing --------------------------------------------------

    /// Runs `utmctl` with `args` and returns stdout, applying T1: stderr
    /// decides, not the exit code.
    async fn utmctl(&self, args: &[&str]) -> Result<String> {
        let utmctl = self.utmctl_path()?;
        let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let cmd = Cmd::new(&utmctl, argv);
        let mut out = self.runner.run(&cmd).await?;
        if is_transient_app_error(&out.stderr) {
            self.app_ready.store(false, Ordering::Relaxed);
            self.ensure_app_running().await?;
            out = self.runner.run(&cmd).await?;
        }
        if out.status != 0 || has_apple_event_error(&out.stderr) {
            return Err(self.classify(&format!("utmctl {}", args.join(" ")), &out));
        }
        Ok(out.stdout)
    }

    /// Runs a trusted AppleScript template with `args` as data
    /// (`osascript - <args>` feeds the script on stdin and hands the rest to
    /// `on run argv`), and returns its result.
    async fn osascript(&self, script: &str, args: &[&str]) -> Result<String> {
        let mut out = self.osascript_raw(script, args).await?;
        if is_transient_app_error(&out.stderr) {
            self.app_ready.store(false, Ordering::Relaxed);
            self.ensure_app_running().await?;
            out = self.osascript_raw(script, args).await?;
        }
        if out.status != 0 || has_apple_event_error(&out.stderr) {
            return Err(self.classify("osascript", &out));
        }
        Ok(out.stdout)
    }

    async fn osascript_raw(&self, script: &str, args: &[&str]) -> Result<Output> {
        let mut argv = vec!["-".to_string()];
        argv.extend(args.iter().map(|a| a.to_string()));
        let mut cmd = Cmd::new("osascript", argv);
        cmd.stdin = Some(script.as_bytes().to_vec());
        self.runner.run(&cmd).await
    }

    /// Turns a failed invocation into the error the user should see, by
    /// error *number* and ASCII prefix only (§1's matching rules).
    fn classify(&self, what: &str, out: &Output) -> WargameError {
        let detail = error_detail(&out.stderr);
        if is_automation_denied(&out.stderr) {
            return WargameError::AutomationDenied {
                app: UTM_APP.to_string(),
                detail,
            };
        }
        if is_vm_not_found(&out.stderr) {
            return WargameError::VmNotFound(detail);
        }
        WargameError::Other(format!("{what} failed: {detail}"))
    }
}

fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_MULTI: &str = include_str!("../tests/fixtures/utmctl-list-multi.txt");
    const LIST_EMPTY: &str = include_str!("../tests/fixtures/utmctl-list-empty.txt");
    const ERR_NOT_RUNNING: &str =
        include_str!("../tests/fixtures/utmctl-error-not-running.stderr.txt");
    const ERR_DENIED: &str =
        include_str!("../tests/fixtures/applescript-error-appleevents-denied.txt");
    const ERR_VM_NOT_FOUND: &str =
        include_str!("../tests/fixtures/utmctl-error-vm-not-found.stderr.txt");

    #[test]
    fn parses_the_real_list_output_including_a_korean_name_with_spaces() {
        let vms = parse_list(LIST_MULTI).unwrap();
        assert_eq!(vms.len(), 4);
        assert_eq!(vms[0].id, "A8E9F979-A198-433A-B2DE-74A6D7302D2F");
        assert_eq!(vms[0].status, UtmStatus::Stopped);
        assert_eq!(vms[0].name, "wargame-t1-x86");
        assert_eq!(vms[3].name, "wargame-t1 한글 이름 vm");
    }

    #[test]
    fn parses_an_empty_listing() {
        assert!(parse_list(LIST_EMPTY).unwrap().is_empty());
    }

    #[test]
    fn rejects_a_table_that_changed_shape() {
        assert!(parse_list("Name Status\nfoo running\n").is_err());
        assert!(parse_list("UUID Status Name\nnot-a-uuid stopped x\n").is_err());
        assert!(parse_list(
            "UUID                                 Status   Name\nA8E9F979-A198-433A-B2DE-74A6D7302D2F confused x\n"
        )
        .is_err());
    }

    #[test]
    fn error_shapes_are_matched_by_code_and_ascii_prefix_only() {
        assert!(has_apple_event_error(ERR_NOT_RUNNING));
        assert!(has_apple_event_error(ERR_VM_NOT_FOUND));
        assert!(is_vm_not_found(ERR_VM_NOT_FOUND));
        assert!(is_automation_denied(ERR_DENIED));
        assert!(!has_apple_event_error("started\n"));
        assert!(!is_automation_denied(ERR_NOT_RUNNING));
        // The fixtures carry typographic apostrophes; nothing here matches
        // prose, so this must hold whatever UTM writes.
        assert!(ERR_NOT_RUNNING.contains('\u{2019}'));
    }

    #[test]
    fn error_codes_are_matched_as_numbers_not_as_substrings() {
        assert!(has_error_code("(OSStatus error -609.)", "-609"));
        assert!(has_error_code("execution error: … (-1712)", "-1712"));
        // A longer code that merely contains ours is not ours.
        assert!(!has_error_code("(OSStatus error -6091.)", "-609"));
        assert!(!has_error_code("(-17120)", "-1712"));
        // Nor is a path or a name that happens to carry the digits.
        assert!(!has_error_code(
            "file.filename=/Users/x/iso/kali-609-installer.iso",
            "-609"
        ));
        assert!(!is_transient_app_error(ERR_NOT_RUNNING));
        assert!(is_transient_app_error(
            "Error from event: … (OSStatus error -609.)"
        ));
    }

    #[test]
    fn the_status_enumeration_is_complete_and_stopping_is_not_live() {
        for state in [
            "stopped", "starting", "started", "pausing", "paused", "resuming", "stopping",
        ] {
            assert!(UtmStatus::parse(state).is_some(), "{state}");
        }
        assert!(UtmStatus::parse("saved").is_none());
        assert!(!UtmStatus::parse("stopping").unwrap().is_live());
        assert!(!UtmStatus::parse("stopped").unwrap().is_live());
        assert!(UtmStatus::parse("paused").unwrap().is_live());
    }

    /// The scripts are constants: they address the VM by UUID and never
    /// interpolate anything.
    #[test]
    fn id_addressed_scripts_look_the_vm_up_by_uuid_never_by_name() {
        let script = vm_script(NETWORK_PROLOGUE, NETWORK_BODY);
        assert!(script.contains("if (id of vm) is vmId then"));
        assert!(!script.contains("virtual machine named"));
        assert!(script.contains(VM_NOT_FOUND));
    }

    /// T5: `POSIX file` has to be resolved before entering the `tell` block.
    #[test]
    fn iso_scripts_resolve_posix_file_outside_the_tell_block() {
        for script in [
            vm_script(ATTACH_ISO_PROLOGUE, ATTACH_ISO_BODY),
            vm_script(REPLACE_ISO_PROLOGUE, REPLACE_ISO_BODY),
            MAKE_WITH_ISO.to_string(),
        ] {
            let posix = script.find("POSIX file").expect("a POSIX file conversion");
            let tell = script.find("tell application").expect("a tell block");
            assert!(posix < tell, "{script}");
        }
    }

    /// `duplicate` needs a single-object specifier: the loop reference every
    /// other verb uses fails with -10014 on UTM 4.7.5. The loop stays (it is
    /// what produces the sentinel), the copy addresses the VM by id.
    #[test]
    fn the_duplicate_script_addresses_the_source_by_id_not_by_the_loop_reference() {
        let script = vm_script(DUPLICATE_PROLOGUE, DUPLICATE_BODY);
        assert!(script.contains("duplicate (first virtual machine whose id is vmId)"));
        assert!(!script.contains("duplicate vm "));
        assert!(!script.contains("virtual machine named"));
        assert!(script.contains(VM_NOT_FOUND));
    }

    /// The one write in this crate that could destroy a user's OS install:
    /// it must be impossible to reach with an empty drive list. Asserted on
    /// the script text, because that is where the decision is made -- no
    /// fake `osascript` can execute the branch.
    #[test]
    fn the_eject_script_never_writes_a_drive_list_with_no_survivor() {
        let script = vm_script("", EJECT_MEDIA_BODY);
        let guard = script
            .find("if keptCount is 0 then")
            .expect("a guard on the number of surviving drives");
        let refusal = script
            .find(&format!("return \"{NO_SURVIVING_DRIVE}\""))
            .expect("the refusal sentinel");
        let write = script
            .find("set drives of c to kept")
            .expect("the drive-list write");
        let update = script
            .find("update configuration of vm with c")
            .expect("the configuration write");
        assert!(guard < refusal, "{script}");
        assert!(
            refusal < write && refusal < update,
            "the refusal has to return before either write: {script}"
        );
        // And the survivor is still echoed back by its own id, or UTM adds
        // a drive instead of keeping one.
        assert!(
            script.contains("set end of kept to {id:(id of d)}"),
            "{script}"
        );
    }

    /// T4: `protocol:TCP` collides with the `serial interface` enumerator.
    /// This adapter sets no port forwards at all, so the trap cannot bite --
    /// assert that rather than leaving it to memory.
    #[test]
    fn no_script_writes_a_port_forward() {
        for script in [
            MAKE_WITH_ISO,
            MAKE_WITHOUT_ISO,
            &vm_script(NETWORK_PROLOGUE, NETWORK_BODY),
        ] {
            assert!(!script.contains("port forwards"), "{script}");
            assert!(!script.contains("protocol:"), "{script}");
        }
    }
}
