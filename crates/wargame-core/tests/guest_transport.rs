//! Task 5: the guest side is OS-aware (spec.md §3 FR-3/FR-4, §7 AC-07/AC-11).
//!
//! Four groups of tests:
//!
//! 1. **PowerShell encoding** -- quoting and `-EncodedCommand` survive
//!    quotes, backslashes, `$`, backticks, spaces, Korean and newlines.
//! 2. **The transport** -- a POSIX endpoint produces byte-identical argv to
//!    the pre-Task-5 code, a PowerShell endpoint produces encoded PowerShell
//!    and Windows destination paths, and readiness names the service that is
//!    missing instead of reporting success.
//! 3. **`GuestOps`** -- the one place a command's OS shape is decided, so no
//!    application service branches on the guest OS itself.
//! 4. **The guest bootstrap scripts** -- the workspace layout, host/guest
//!    SHA-256 agreement, and the archive-safety rules of AC-11, exercised
//!    against real (hand-built) malicious archives.
//!
//! No test here touches a real guest: Task 7 owns the first real Windows and
//! Kali installs.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use wargame_core::domain::{GuestEndpoint, GuestShell};
use wargame_core::error::WargameError;
use wargame_core::infra::powershell::{self, PowerShellCommand};
use wargame_core::infra::process::Output;
use wargame_core::infra::{GuestCommand, GuestOps, SshGuestTransport};
use wargame_core::ports::GuestTransport;
use wargame_core::testing::FakeRunner;

fn posix_endpoint(user: &str) -> GuestEndpoint {
    GuestEndpoint::new(IpAddr::from_str("10.0.0.5").unwrap(), user)
}

fn windows_endpoint() -> GuestEndpoint {
    GuestEndpoint {
        host: IpAddr::from_str("10.0.0.9").unwrap(),
        port: 22,
        user: "wargame".to_string(),
        key_path: None,
        shell: GuestShell::PowerShell,
    }
}

fn transport(runner: &Arc<FakeRunner>) -> SshGuestTransport {
    SshGuestTransport::new(runner.clone(), PathBuf::from("/keys/id_ed25519"))
}

// ---------------------------------------------------------------------------
// 1. PowerShell encoding
// ---------------------------------------------------------------------------

/// Every awkward character the spec names has to survive the round trip
/// (spec.md §3 FR-4: "Windows 경로, 공백, 작은따옴표·큰따옴표·한글 경로를
/// 지원한다").
#[test]
fn encoded_commands_round_trip_quotes_backslashes_korean_and_newlines() {
    let awkward = [
        "plain",
        "it's",
        r#"say "hi""#,
        r"C:\Users\wargame\wargame",
        "$env:PATH",
        "back`tick",
        "with spaces",
        "한글 경로",
        "line one\nline two",
        r#"C:\Users\사용자\wargame\it's "here""#,
    ];
    for value in awkward {
        let encoded = powershell::encode_command(value);
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='),
            "-EncodedCommand must be plain base64, got {encoded:?}"
        );
        assert_eq!(
            powershell::decode_command(&encoded).as_deref(),
            Some(value),
            "round trip failed for {value:?}"
        );
    }
}

/// The literal quoting is PowerShell's, not POSIX's: single quotes are
/// doubled, and nothing else is touched (so `$`, backticks and backslashes
/// stay literal).
#[test]
fn powershell_quoting_is_not_posix_quoting() {
    assert_eq!(powershell::quote("plain"), "'plain'");
    assert_eq!(powershell::quote("it's"), "'it''s'");
    assert_eq!(
        powershell::quote(r"C:\Users\사용자\wargame"),
        r"'C:\Users\사용자\wargame'"
    );
    assert_eq!(powershell::quote("$env:PATH"), "'$env:PATH'");
    assert_eq!(powershell::quote("back`tick"), "'back`tick'");
    // POSIX's `'\''` escape must never appear.
    assert!(!powershell::quote("it's").contains('\\'));
}

/// The builder takes argv-like data and produces a script that calls it
/// through the call operator, propagating the command's exit code (so a
/// Windows installer's 3010 "reboot required" reaches the host).
#[test]
fn the_builder_calls_argv_and_propagates_the_exit_code() {
    let argv = vec![
        "powershell".to_string(),
        "-File".to_string(),
        r#"C:\Users\사용자\wg-provision\it's "here".ps1"#.to_string(),
    ];
    let command = PowerShellCommand::from_argv(&argv);
    let script = command.script().to_string();

    assert!(
        script
            .contains(r#"& 'powershell' '-File' 'C:\Users\사용자\wg-provision\it''s "here".ps1'"#),
        "script did not quote argv as PowerShell literals:\n{script}"
    );
    assert!(
        script.contains("$wgExit = $LASTEXITCODE") && script.contains("exit $wgExit"),
        "the guest command's exit code must reach the host:\n{script}"
    );
    assert_eq!(
        powershell::decode_command(&command.encoded()).as_deref(),
        Some(script.as_str())
    );
    assert_eq!(
        command.remote_argv()[..4],
        [
            "powershell".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-EncodedCommand".to_string(),
        ]
    );
}

// ---------------------------------------------------------------------------
// 2. The transport
// ---------------------------------------------------------------------------

/// The exact argv the pre-Task-5 transport produced for the macOS path. A
/// change here is a change to a verified, working real-VM path, so it is
/// pinned byte for byte rather than probed with `contains`.
#[tokio::test]
async fn posix_argv_is_byte_identical_to_the_pre_task_5_transport() {
    let runner = Arc::new(FakeRunner::new());
    let transport = transport(&runner);
    let endpoint = posix_endpoint("admin");

    transport
        .exec(&endpoint, &["echo".to_string(), "hi there".to_string()])
        .await
        .unwrap();
    transport
        .copy_in(&endpoint, Path::new("/tmp/a.zip"), "~/wargame/incoming/")
        .await
        .unwrap();
    transport
        .wait_ready(&endpoint, Duration::from_secs(30))
        .await
        .unwrap();

    let base = vec![
        "-i",
        "/keys/id_ed25519",
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/dev/null",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "LogLevel=ERROR",
        "-o",
        "BatchMode=yes",
    ];

    let calls = runner.calls();
    assert_eq!(calls.len(), 3);

    let mut expected_exec = base.clone();
    expected_exec.extend(["admin@10.0.0.5", "--", "'echo' 'hi there'"]);
    assert_eq!(calls[0].program, "ssh");
    assert_eq!(calls[0].args, expected_exec);

    let mut expected_copy = base.clone();
    expected_copy.extend(["-r", "/tmp/a.zip", "admin@10.0.0.5:~/wargame/incoming/"]);
    assert_eq!(calls[1].program, "scp");
    assert_eq!(calls[1].args, expected_copy);

    let mut expected_probe = base.clone();
    expected_probe.extend(["admin@10.0.0.5", "true"]);
    assert_eq!(calls[2].program, "ssh");
    assert_eq!(calls[2].args, expected_probe);
}

/// A PowerShell guest gets its command base64-encoded, never quoted the
/// POSIX way (spec.md §3 FR-4: "Unix shell quoting을 Windows에 재사용하지
/// 않는다").
#[tokio::test]
async fn windows_exec_sends_an_encoded_powershell_command() {
    let runner = Arc::new(FakeRunner::new());
    let transport = transport(&runner);

    transport
        .exec(
            &windows_endpoint(),
            &[
                "powershell".to_string(),
                "-File".to_string(),
                r#"C:\Users\사용자\wg-provision\it's "here".ps1"#.to_string(),
            ],
        )
        .await
        .unwrap();

    let call = runner.calls().remove(0);
    assert_eq!(call.program, "ssh");
    let tail = &call.args[call.args.len() - 5..];
    assert_eq!(
        tail[..4],
        [
            "powershell".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-EncodedCommand".to_string(),
        ]
    );
    let script = powershell::decode_command(&tail[4]).expect("the last argv element is base64");
    assert!(
        script.contains(r#"'C:\Users\사용자\wg-provision\it''s "here".ps1'"#),
        "the Windows path did not survive encoding:\n{script}"
    );
    assert!(
        !call.args.iter().any(|a| a.contains(r"'\''")),
        "POSIX single-quote escaping must never appear on a Windows command line: {:?}",
        call.args
    );
    assert!(
        call.args.contains(&"wargame@10.0.0.9".to_string()),
        "{:?}",
        call.args
    );
}

/// Windows destinations keep their drive letter, backslashes, spaces,
/// quotes and Korean characters exactly as the guest will see them, and a
/// non-default port and key still come from the endpoint.
#[tokio::test]
async fn windows_copy_in_uses_the_windows_destination_verbatim() {
    let runner = Arc::new(FakeRunner::new());
    let transport = transport(&runner);
    let endpoint = GuestEndpoint {
        host: IpAddr::from_str("127.0.0.1").unwrap(),
        port: 22022,
        user: "사용자".to_string(),
        key_path: Some(PathBuf::from("/keys/windows_ed25519")),
        shell: GuestShell::PowerShell,
    };
    let dest = r#"C:\Users\사용자\wargame\it's "here"\"#;

    transport
        .copy_in(&endpoint, Path::new("/tmp/a.zip"), dest)
        .await
        .unwrap();

    let call = runner.calls().remove(0);
    assert_eq!(call.program, "scp");
    assert_eq!(call.args[1], "/keys/windows_ed25519");
    assert!(call.args.contains(&"-P".to_string()));
    assert!(call.args.contains(&"22022".to_string()));
    assert!(
        !call.args.contains(&"-O".to_string()),
        "scp must stay on the SFTP protocol so the guest's shell never sees the path"
    );
    assert_eq!(
        *call.args.last().unwrap(),
        format!("사용자@127.0.0.1:{dest}")
    );
}

/// `wargame ssh` into a Windows session lands in PowerShell, not whatever
/// the guest's default SSH shell happens to be.
#[test]
fn the_interactive_shell_lands_in_powershell_for_a_windows_session() {
    let runner = Arc::new(FakeRunner::new());
    let transport = transport(&runner);

    let posix = transport.interactive_shell_cmd(&posix_endpoint("admin"));
    let posix_args: Vec<String> = posix
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(posix.get_program().to_string_lossy(), "ssh");
    assert_eq!(*posix_args.last().unwrap(), "admin@10.0.0.5");

    let windows = transport.interactive_shell_cmd(&windows_endpoint());
    let windows_args: Vec<String> = windows
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        windows_args.iter().any(|a| a == "powershell"),
        "a Windows session must land in PowerShell: {windows_args:?}"
    );
    let host_index = windows_args
        .iter()
        .position(|a| a == "wargame@10.0.0.9")
        .expect("the endpoint's user@host");
    let shell_index = windows_args.iter().position(|a| a == "powershell").unwrap();
    assert!(
        host_index < shell_index,
        "the remote shell must follow the destination: {windows_args:?}"
    );
}

/// A guest whose SSH never answers, one that answers but refuses the key,
/// and one that authenticates but cannot run the shell are three different
/// problems, and none of them is success (spec.md §6's "SSH/guest agent
/// 미준비: 필요한 서비스를 구분하고 단계·timeout 표시").
#[tokio::test]
async fn readiness_names_the_service_that_is_missing() {
    async fn probe_failure(status: i32, stderr: &'static str) -> WargameError {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(move |_| {
            Ok(Output {
                status,
                stdout: String::new(),
                stderr: stderr.to_string(),
            })
        });
        transport(&runner)
            .wait_ready(&windows_endpoint(), Duration::from_millis(1))
            .await
            .expect_err("a guest that never answers is never ready")
    }

    let unreachable = probe_failure(
        255,
        "ssh: connect to host 10.0.0.9 port 22: Connection refused",
    )
    .await;
    let message = unreachable.to_string();
    assert!(message.contains("SSH"), "{message}");
    assert!(
        message.contains("0.0 s") || message.contains("elapsed"),
        "{message}"
    );
    assert!(
        message.contains("1"),
        "the timeout used must be shown: {message}"
    );

    let unauthenticated =
        probe_failure(255, "wargame@10.0.0.9: Permission denied (publickey).").await;
    let message = unauthenticated.to_string();
    assert!(
        message.contains("authenticate") || message.contains("key"),
        "a refused key must not read as an unreachable host: {message}"
    );

    let no_shell = probe_failure(
        9009,
        "'powershell' is not recognized as an internal or external command",
    )
    .await;
    let message = no_shell.to_string();
    assert!(
        message.to_lowercase().contains("powershell"),
        "a guest whose shell does not answer must say so: {message}"
    );
    assert!(
        no_shell.hint().is_some_and(|h| h.contains("OpenSSH")),
        "the hint must say how to prepare the missing service"
    );
}

/// The readiness probe itself has to speak the guest's shell: `true` is not
/// a Windows command.
#[tokio::test]
async fn the_readiness_probe_speaks_the_guests_shell() {
    let runner = Arc::new(FakeRunner::new());
    transport(&runner)
        .wait_ready(&windows_endpoint(), Duration::from_secs(1))
        .await
        .unwrap();

    let call = runner.calls().remove(0);
    assert!(
        !call.args.contains(&"true".to_string()),
        "`true` is a POSIX command: {:?}",
        call.args
    );
    assert!(call.args.contains(&"-EncodedCommand".to_string()));
}

// ---------------------------------------------------------------------------
// 3. GuestOps -- the single place the OS decides a command's shape
// ---------------------------------------------------------------------------

fn argv_of(command: &GuestCommand) -> Vec<String> {
    command
        .as_argv()
        .unwrap_or_else(|| panic!("expected argv, got {command:?}"))
        .to_vec()
}

fn script_of(command: &GuestCommand) -> String {
    match command {
        GuestCommand::Script(script) => script.clone(),
        other => panic!("expected a script, got {other:?}"),
    }
}

#[test]
fn posix_guest_ops_produce_the_commands_the_mvp_already_ran() {
    let ops = GuestOps::new(GuestShell::Posix);
    let mkdir = ops.mkdir_p(&["/a".to_string(), "/b".to_string()]);
    assert_eq!(mkdir.len(), 1);
    assert_eq!(argv_of(&mkdir[0]), ["mkdir", "-p", "/a", "/b"]);
    assert_eq!(
        argv_of(&ops.remove_tree("wg-provision")),
        ["rm", "-rf", "wg-provision"]
    );
    assert_eq!(argv_of(&ops.flush()), ["sync"]);
    assert_eq!(argv_of(&ops.probe()), ["true"]);
    assert_eq!(
        argv_of(&ops.run_script("wg-provision/scripts/macos/common.sh")),
        ["bash", "wg-provision/scripts/macos/common.sh"]
    );
    assert_eq!(ops.bootstrap_name(), "bootstrap.sh");
    assert_eq!(
        argv_of(&ops.run_bootstrap(
            "/Users/admin/wargame/.wg/bootstrap.sh",
            "/Users/admin/wargame/s",
            true,
            false
        )),
        [
            "bash",
            "/Users/admin/wargame/.wg/bootstrap.sh",
            "/Users/admin/wargame/s",
            "--extract=true"
        ]
    );
    assert_eq!(
        ops.join("/Users/admin/wargame/", "incoming"),
        "/Users/admin/wargame/incoming"
    );
    assert_eq!(
        ops.dir_dest("/Users/admin/wargame/incoming"),
        "/Users/admin/wargame/incoming/"
    );
}

#[test]
fn powershell_guest_ops_use_windows_commands_and_separators() {
    let ops = GuestOps::new(GuestShell::PowerShell);

    // A cmdlet binds a parameter only from a bare `-Name` token, so these
    // arrive as a trusted template with the parameter names unquoted and
    // only the data quoted -- never as argv of quoted strings, which
    // PowerShell would bind positionally.
    let mkdir = ops.mkdir_p(&[
        r"C:\wargame\.wg".to_string(),
        r"C:\wargame\s\incoming".to_string(),
    ]);
    assert_eq!(
        mkdir.len(),
        2,
        "New-Item takes one -Path per call: {mkdir:?}"
    );
    assert_eq!(
        script_of(&mkdir[0]),
        r"New-Item -ItemType Directory -Force -Path 'C:\wargame\.wg' | Out-Null"
    );

    assert_eq!(
        script_of(&ops.remove_tree("wg-provision")),
        "Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -LiteralPath 'wg-provision'"
    );
    assert_eq!(script_of(&ops.flush()), "Write-VolumeCache -DriveLetter C");
    assert!(!script_of(&ops.probe()).contains("true"));
    assert_eq!(ops.bootstrap_name(), "bootstrap.ps1");

    // `powershell.exe` is a *native* command, so its arguments are ordinary
    // literals and argv is the right shape.
    let run = argv_of(&ops.run_script(r"wg-provision\scripts\windows\common.ps1"));
    assert_eq!(run[0], "powershell");
    assert!(run.iter().any(|a| a == "-File"), "{run:?}");
    assert!(run.iter().any(|a| a == "-ExecutionPolicy"), "{run:?}");

    let bootstrap = argv_of(&ops.run_bootstrap(
        r"C:\wargame\.wg\bootstrap.ps1",
        r"C:\wargame\s",
        false,
        true,
    ));
    assert!(bootstrap.iter().any(|a| a == "-Workspace"), "{bootstrap:?}");
    assert!(bootstrap.iter().any(|a| a == "false"), "{bootstrap:?}");
    assert!(
        bootstrap.iter().any(|a| a == "-RequireDigests"),
        "{bootstrap:?}"
    );
    assert!(
        !bootstrap.iter().any(|a| a.starts_with("--extract")),
        "POSIX flag syntax must not be reused: {bootstrap:?}"
    );

    assert_eq!(
        ops.join(r"C:\wargame", "s/incoming"),
        r"C:\wargame\s\incoming"
    );
    assert_eq!(ops.join(r"C:\wargame\", "incoming"), r"C:\wargame\incoming");
    assert_eq!(
        ops.dir_dest(r"C:\wargame\incoming"),
        r"C:\wargame\incoming\"
    );
    assert_eq!(ops.trim_root(r"C:\wargame\"), r"C:\wargame");
    assert_eq!(
        GuestOps::new(GuestShell::Posix).trim_root("/home/kali/wargame/"),
        "/home/kali/wargame"
    );
}

/// A cmdlet script reaches the guest as a script, with its parameter names
/// bare and only its data quoted. PowerShell binds a parameter *only* from
/// an unquoted `-Name` token, so `& 'New-Item' '-Force'` would pass
/// `-Force` as a positional value and misbind everything after it.
#[tokio::test]
async fn cmdlet_parameter_names_reach_the_guest_unquoted() {
    let runner = Arc::new(FakeRunner::new());
    let transport = transport(&runner);
    let ops = GuestOps::new(GuestShell::PowerShell);

    ops.mkdir_p(&[r"C:\wargame\it's here".to_string()])[0]
        .run(&transport, &windows_endpoint())
        .await
        .unwrap();

    let call = runner.calls().remove(0);
    let script = powershell::decode_command(call.args.last().unwrap()).expect("base64");
    assert!(
        script.contains(r"New-Item -ItemType Directory -Force -Path 'C:\wargame\it''s here'"),
        "parameter names must be bare and only the data quoted:\n{script}"
    );
    for parameter in ["'-ItemType'", "'-Force'", "'-Path'", "'-Recurse'"] {
        assert!(
            !script.contains(parameter),
            "a quoted {parameter} would bind positionally:\n{script}"
        );
    }
    // The wrapper around a cmdlet template keeps `Stop`, so a failed
    // New-Item cannot be mistaken for success.
    assert!(
        script.contains("$ErrorActionPreference = 'Stop'"),
        "{script}"
    );
}

/// A native command keeps the argv path, and its wrapper must *not* use
/// `Stop`: in Windows PowerShell 5.1 a native command that writes to stderr
/// with redirected streams raises NativeCommandError under `Stop`, which
/// would be caught and reported as exit 1 -- masking the real exit code.
#[tokio::test]
async fn a_native_commands_stderr_cannot_mask_its_exit_code() {
    let runner = Arc::new(FakeRunner::new());
    let transport = transport(&runner);

    transport
        .exec(
            &windows_endpoint(),
            &[
                "powershell".to_string(),
                "-File".to_string(),
                "a.ps1".to_string(),
            ],
        )
        .await
        .unwrap();

    let call = runner.calls().remove(0);
    let script = powershell::decode_command(call.args.last().unwrap()).expect("base64");
    assert!(
        script.contains("$ErrorActionPreference = 'Continue'"),
        "a native command must not run under 'Stop':\n{script}"
    );
}

/// `ssh` can only exit with the low byte of the remote status, so 3010
/// would arrive as 194. The wrapper reports an out-of-range code on a marker
/// line and the transport puts it back -- and takes the line out of stderr.
#[tokio::test]
async fn an_out_of_range_guest_exit_code_survives_ssh() {
    let runner = Arc::new(FakeRunner::new());
    runner.set_responder(|_| {
        Ok(Output {
            status: 255,
            stdout: String::new(),
            stderr: format!(
                "installer said reboot\n{}3010\n",
                powershell::EXIT_CODE_MARKER
            ),
        })
    });
    let marked = transport(&runner);

    let out = marked
        .exec(&windows_endpoint(), &["powershell".to_string()])
        .await
        .unwrap();
    assert_eq!(
        out.status, 3010,
        "3010 must not arrive as 3010 & 0xFF = 194"
    );
    assert!(
        !out.stderr.contains(powershell::EXIT_CODE_MARKER),
        "the marker must never reach a log or a user: {:?}",
        out.stderr
    );
    assert!(
        out.stderr.contains("installer said reboot"),
        "{:?}",
        out.stderr
    );

    // The wrapper is what emits it.
    let script =
        powershell::decode_command(runner.calls().remove(0).args.last().unwrap()).expect("base64");
    assert!(script.contains(powershell::EXIT_CODE_MARKER), "{script}");
    assert!(script.contains("-gt 255"), "{script}");

    // A genuine 255 with no marker stays 255.
    let runner = Arc::new(FakeRunner::new());
    runner.set_responder(|_| {
        Ok(Output {
            status: 255,
            stdout: String::new(),
            stderr: "ssh: connect to host 10.0.0.9 port 22: Connection refused\n".to_string(),
        })
    });
    let plain = transport(&runner);
    let out = plain
        .exec(&windows_endpoint(), &["powershell".to_string()])
        .await
        .unwrap();
    assert_eq!(out.status, 255);
}

// ---------------------------------------------------------------------------
// 4. The guest bootstrap scripts
// ---------------------------------------------------------------------------

fn guest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../guest")
}

fn have(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("--version")
        .output()
        .map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty())
        .unwrap_or(false)
}

/// macOS guests run `/bin/bash`, which is still bash 3.2, so the scripts are
/// exercised through it whenever it exists rather than through whatever
/// newer bash happens to be first on `PATH`.
fn bash() -> &'static str {
    if Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "bash"
    }
}

fn run_bootstrap_sh(workspace: &Path, extract: bool) -> std::process::Output {
    std::process::Command::new(bash())
        .arg(guest_dir().join("bootstrap.sh"))
        .arg(workspace)
        .arg(format!("--extract={extract}"))
        .output()
        .expect("failed to spawn bash")
}

/// `bootstrap.sh --check-entry <name>` / `bootstrap.ps1 -CheckEntry <name>`
/// expose the archive-entry rule the scripts apply, so the same table can be
/// asserted against both implementations.
fn check_entry_sh(name: &str) -> std::process::Output {
    std::process::Command::new(bash())
        .arg(guest_dir().join("bootstrap.sh"))
        .arg("--check-entry")
        .arg(name)
        .output()
        .expect("failed to spawn bash")
}

/// (entry name, is it safe to extract?)
const ENTRY_RULES: &[(&str, bool)] = &[
    ("hello.txt", true),
    ("sub/dir/file.txt", true),
    ("./ok.txt", true),
    ("a.b.c", true),
    ("", false),
    ("/etc/passwd", false),
    ("../evil.txt", false),
    ("a/../../evil.txt", false),
    ("..", false),
    (r"C:\evil.txt", false),
    ("C:/evil.txt", false),
    (r"\\server\share\evil.txt", false),
    (r"\evil.txt", false),
    (r"..\evil.txt", false),
    (r"a\..\..\evil.txt", false),
    ("CON", false),
    ("con.txt", false),
    ("sub/NUL", false),
    ("LPT1", false),
    ("file.", false),
    ("file ", false),
    ("a/b./c.txt", false),
    ("stream:name", false),
];

#[test]
fn the_bash_bootstrap_applies_the_archive_entry_rules() {
    if !have("bash") {
        eprintln!("skipping: bash not on PATH");
        return;
    }
    for (name, safe) in ENTRY_RULES {
        let out = check_entry_sh(name);
        assert_eq!(
            out.status.success(),
            *safe,
            "entry {name:?} should be {}: {}{}",
            if *safe { "accepted" } else { "rejected" },
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        if !*safe {
            assert!(
                !String::from_utf8_lossy(&out.stderr).trim().is_empty(),
                "a rejection must say why: {name:?}"
            );
        }
    }
}

#[test]
fn the_powershell_bootstrap_applies_the_same_archive_entry_rules() {
    let Some(pwsh) = ["pwsh", "powershell"].into_iter().find(|b| have(b)) else {
        eprintln!("skipping: no PowerShell on PATH (Task 7 runs this on a real guest)");
        return;
    };
    for (name, safe) in ENTRY_RULES {
        let out = std::process::Command::new(pwsh)
            .arg("-NoProfile")
            .arg("-File")
            .arg(guest_dir().join("bootstrap.ps1"))
            .arg("-CheckEntry")
            .arg(name)
            .output()
            .expect("failed to spawn PowerShell");
        assert_eq!(
            out.status.success(),
            *safe,
            "entry {name:?} should be {}: {}{}",
            if *safe { "accepted" } else { "rejected" },
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

/// Both scripts have to exist and declare the same workspace contract.
#[test]
fn both_bootstrap_scripts_create_the_same_workspace_tree() {
    let ps1 = std::fs::read_to_string(guest_dir().join("bootstrap.ps1")).expect("bootstrap.ps1");
    for dir in ["incoming", "work", "notes", "output"] {
        assert!(ps1.contains(dir), "bootstrap.ps1 must create {dir}/");
    }
    assert!(
        ps1.contains("SHA256") || ps1.contains("sha256"),
        "bootstrap.ps1 must verify the host's SHA-256 digests"
    );
    assert!(
        !ps1.contains("Invoke-Expression"),
        "the bootstrap never evaluates guest data as code"
    );

    if !have("bash") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    let out = run_bootstrap_sh(&ws, false);
    assert!(
        out.status.success(),
        "bootstrap.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for dir in ["incoming", "work", "notes", "output"] {
        assert!(ws.join(dir).is_dir(), "missing {dir}/");
    }
    assert!(ws.join("notes/README.md").is_file());
}

/// A digest list the host staged but the guest never received must fail the
/// bootstrap, not quietly downgrade to no verification at all.
#[test]
fn a_missing_digest_list_is_an_error_when_the_host_staged_one() {
    if !have("bash") {
        eprintln!("skipping: bash not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("incoming")).unwrap();
    std::fs::write(ws.join("incoming/challenge.bin"), b"hi\n").unwrap();

    // Without the flag (a manual run) an absent list is simply nothing to
    // check.
    let out = run_bootstrap_sh(&ws, false);
    assert!(out.status.success());

    let out = std::process::Command::new(bash())
        .arg(guest_dir().join("bootstrap.sh"))
        .arg(&ws)
        .arg("--extract=true")
        .arg("--require-digests")
        .output()
        .expect("failed to spawn bash");
    assert!(
        !out.status.success(),
        "a digest list the host staged must be present"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(".wargame-sha256"), "{stderr}");
    assert!(
        !ws.join("incoming/extracted").exists(),
        "nothing may be extracted when verification could not run"
    );

    // The PowerShell half declares the same switch.
    let ps1 = std::fs::read_to_string(guest_dir().join("bootstrap.ps1")).unwrap();
    assert!(
        ps1.contains("$RequireDigests"),
        "bootstrap.ps1 needs the switch"
    );
}

/// FR-4: "게스트 파일 해시는 호스트 staging 해시와 같아야 한다" -- the guest
/// checks what the host recorded, and a mismatch fails the bootstrap instead
/// of handing over a corrupted challenge file.
#[test]
fn the_guest_verifies_the_hosts_sha256_digests() {
    if !have("bash") {
        eprintln!("skipping: bash not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("incoming")).unwrap();
    std::fs::write(ws.join("incoming/challenge.bin"), b"hi\n").unwrap();
    // sha256("hi\n"), in the `sha256sum` layout the host stages.
    let digest = "98ea6e4f216f2fb4b69fff9b3a44842c38686ca685f3f55dc48c5d3fb1107be4";
    std::fs::write(
        ws.join("incoming/.wargame-sha256"),
        format!("{digest}  challenge.bin\n"),
    )
    .unwrap();

    let out = run_bootstrap_sh(&ws, false);
    assert!(
        out.status.success(),
        "a matching digest must pass: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    std::fs::write(ws.join("incoming/challenge.bin"), b"tampered\n").unwrap();
    let out = run_bootstrap_sh(&ws, false);
    assert!(
        !out.status.success(),
        "a mismatched digest must fail the bootstrap"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("challenge.bin"),
        "the failure must name the file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// --- malicious archive fixtures (AC-11) ------------------------------------

mod archive {
    //! Minimal, dependency-free zip/tar writers, so a fixture can hold entry
    //! names and link types the host's own `zip`/`tar` would refuse to
    //! create. Nothing here is ever extracted on the host.

    use std::path::Path;

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for byte in data {
            crc ^= *byte as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    /// A stored (uncompressed) zip whose entries are written verbatim.
    /// `mode` is the unix mode placed in the external attributes, so a
    /// symlink entry (`0o120777`) can be produced.
    pub fn zip(path: &Path, entries: &[(&str, &[u8], u32)]) {
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        for (name, data, mode) in entries {
            let offset = out.len() as u32;
            let crc = crc32(data);
            let name_bytes = name.as_bytes();
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
            out.extend_from_slice(&0u16.to_le_bytes()); // stored
            out.extend_from_slice(&0u16.to_le_bytes()); // time
            out.extend_from_slice(&0u16.to_le_bytes()); // date
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // extra
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(data);

            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&0x031eu16.to_le_bytes()); // made by unix
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0x0800u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes()); // extra
            central.extend_from_slice(&0u16.to_le_bytes()); // comment
            central.extend_from_slice(&0u16.to_le_bytes()); // disk
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            central.extend_from_slice(&(mode << 16).to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name_bytes);
        }
        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        std::fs::write(path, out).unwrap();
    }

    /// `typeflag`: `b'0'` regular, `b'2'` symlink, `b'1'` hard link.
    pub struct TarEntry<'a> {
        pub name: &'a str,
        pub typeflag: u8,
        pub linkname: &'a str,
        pub data: &'a [u8],
    }

    pub fn tar(path: &Path, entries: &[TarEntry<'_>]) {
        let mut out: Vec<u8> = Vec::new();
        for entry in entries {
            let mut header = [0u8; 512];
            let put = |header: &mut [u8; 512], at: usize, value: &[u8]| {
                header[at..at + value.len()].copy_from_slice(value);
            };
            put(&mut header, 0, entry.name.as_bytes());
            put(&mut header, 100, b"0000644\0");
            put(&mut header, 108, b"0000000\0");
            put(&mut header, 116, b"0000000\0");
            let size = if entry.typeflag == b'0' {
                entry.data.len()
            } else {
                0
            };
            put(&mut header, 124, format!("{size:011o}\0").as_bytes());
            put(&mut header, 136, b"00000000000\0");
            put(&mut header, 148, b"        "); // checksum placeholder
            header[156] = entry.typeflag;
            put(&mut header, 157, entry.linkname.as_bytes());
            put(&mut header, 257, b"ustar\0");
            put(&mut header, 263, b"00");
            let checksum: u32 = header.iter().map(|b| *b as u32).sum();
            put(&mut header, 148, format!("{checksum:06o}\0 ").as_bytes());
            out.extend_from_slice(&header);
            if size > 0 {
                out.extend_from_slice(entry.data);
                out.resize(out.len().div_ceil(512) * 512, 0);
            }
        }
        out.extend_from_slice(&[0u8; 1024]);
        std::fs::write(path, out).unwrap();
    }
}

/// Sets up a workspace with one archive in `incoming/`, runs the bootstrap
/// with extraction on, and returns the process output.
fn extract_fixture(
    name: &str,
    build: impl FnOnce(&Path),
) -> (tempfile::TempDir, PathBuf, std::process::Output) {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("incoming")).unwrap();
    build(&ws.join("incoming").join(name));
    let out = run_bootstrap_sh(&ws, true);
    (tmp, ws, out)
}

/// A well-formed archive still unpacks -- the safety rules must not turn
/// extraction off altogether.
#[test]
fn a_safe_archive_is_extracted_in_the_guest() {
    if !(have("bash") && have("unzip")) {
        eprintln!("skipping: bash/unzip not on PATH");
        return;
    }
    let (_tmp, ws, out) = extract_fixture("challenge.zip", |path| {
        archive::zip(path, &[("hello.txt", b"hi\n", 0o100644)]);
    });
    assert!(
        out.status.success(),
        "bootstrap.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(ws.join("incoming/extracted/challenge/hello.txt").is_file());
}

/// Every malicious shape AC-11 names: the archive is kept, nothing is
/// extracted, and the reason is reported.
#[test]
fn malicious_archives_are_refused_whole_and_kept() {
    if !(have("bash") && have("unzip") && have("tar")) {
        eprintln!("skipping: bash/unzip/tar not on PATH");
        return;
    }

    let zip_cases: &[(&str, &str)] = &[
        ("parent traversal", "../evil.txt"),
        ("absolute path", "/tmp/wargame-evil.txt"),
        ("drive letter", r"C:\wargame-evil.txt"),
        ("UNC path", r"\\server\share\evil.txt"),
        ("backslash traversal", r"..\evil.txt"),
        ("reserved device name", "CON"),
        ("trailing dot", "evil."),
    ];
    for (what, entry) in zip_cases {
        let (_tmp, ws, out) = extract_fixture("challenge.zip", |path| {
            archive::zip(
                path,
                &[
                    ("hello.txt", b"hi\n", 0o100644),
                    (entry, b"pwned\n", 0o100644),
                ],
            );
        });
        assert!(
            !ws.join("incoming/extracted").exists(),
            "{what}: nothing may be extracted from a rejected archive"
        );
        assert!(
            ws.join("incoming/challenge.zip").is_file(),
            "{what}: the original archive must be kept"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            combined.contains("challenge.zip"),
            "{what}: the refusal must name the archive: {combined}"
        );
    }

    // A zip symlink entry (unix mode 0o120777 in the external attributes).
    let (_tmp, ws, _out) = extract_fixture("links.zip", |path| {
        archive::zip(path, &[("link", b"/etc/passwd", 0o120777)]);
    });
    assert!(
        !ws.join("incoming/extracted").exists(),
        "a zip symlink entry must be refused"
    );

    use archive::TarEntry;
    let tar_cases: &[(&str, TarEntry<'_>)] = &[
        (
            "parent traversal",
            TarEntry {
                name: "../evil.txt",
                typeflag: b'0',
                linkname: "",
                data: b"pwned\n",
            },
        ),
        (
            "absolute path",
            TarEntry {
                name: "/tmp/wargame-evil.txt",
                typeflag: b'0',
                linkname: "",
                data: b"pwned\n",
            },
        ),
        (
            "symlink out of the workspace",
            TarEntry {
                name: "link",
                typeflag: b'2',
                linkname: "/etc/passwd",
                data: b"",
            },
        ),
        (
            "relative symlink out of the workspace",
            TarEntry {
                name: "link",
                typeflag: b'2',
                linkname: "../../outside",
                data: b"",
            },
        ),
        (
            "hard link out of the workspace",
            TarEntry {
                name: "link",
                typeflag: b'1',
                linkname: "../../outside",
                data: b"",
            },
        ),
        (
            "drive letter",
            TarEntry {
                name: r"C:\evil.txt",
                typeflag: b'0',
                linkname: "",
                data: b"pwned\n",
            },
        ),
    ];
    for (what, entry) in tar_cases {
        let (_tmp, ws, out) = extract_fixture("challenge.tar", |path| {
            archive::tar(
                path,
                &[
                    TarEntry {
                        name: "hello.txt",
                        typeflag: b'0',
                        linkname: "",
                        data: b"hi\n",
                    },
                    TarEntry {
                        name: entry.name,
                        typeflag: entry.typeflag,
                        linkname: entry.linkname,
                        data: entry.data,
                    },
                ],
            );
        });
        assert!(
            !ws.join("incoming/extracted").exists(),
            "{what}: nothing may be extracted from a rejected archive: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            ws.join("incoming/challenge.tar").is_file(),
            "{what}: the original archive must be kept"
        );
    }

    // Nothing escaped anywhere on the host.
    assert!(!Path::new("/tmp/wargame-evil.txt").exists());
    assert!(!Path::new("/wargame-evil.txt").exists());
}

/// `bootstrap.sh` must work on both POSIX guests it is shipped for, whose
/// tools differ: macOS has `shasum` and bsdtar, Kali/Debian has `sha256sum`
/// and GNU tar. The default test run uses the Nix shell's GNU tools; this
/// one pins the macOS set explicitly.
#[test]
fn the_bash_bootstrap_works_with_the_macos_tool_set() {
    if !Path::new("/usr/bin/shasum").exists() || !Path::new("/usr/bin/unzip").exists() {
        eprintln!("skipping: the macOS tool set is not present");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("incoming")).unwrap();
    std::fs::write(ws.join("incoming/challenge.bin"), b"hi\n").unwrap();
    std::fs::write(
        ws.join("incoming/.wargame-sha256"),
        "98ea6e4f216f2fb4b69fff9b3a44842c38686ca685f3f55dc48c5d3fb1107be4  challenge.bin\n",
    )
    .unwrap();
    archive::zip(
        &ws.join("incoming/challenge.zip"),
        &[("hello.txt", b"hi\n", 0o100644)],
    );

    // `/usr/bin:/bin` is exactly what a stock macOS guest has: bsdtar,
    // shasum (no sha256sum), unzip, bash 3.2.
    let out = std::process::Command::new("/bin/bash")
        .env("PATH", "/usr/bin:/bin")
        .arg(guest_dir().join("bootstrap.sh"))
        .arg(&ws)
        .arg("--extract=true")
        .output()
        .expect("failed to spawn /bin/bash");
    assert!(
        out.status.success(),
        "bootstrap.sh failed with the macOS tool set: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(ws.join("incoming/extracted/challenge/hello.txt").is_file());
    assert!(ws.join("notes/README.md").is_file());
}

/// A guest with no working `python3` still gets its notes and a successful
/// bootstrap: the metadata summary is a convenience, never a launch
/// failure.
#[test]
fn a_guest_without_python3_still_bootstraps() {
    if !have("bash") {
        eprintln!("skipping: bash not on PATH");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("workspace");
    std::fs::create_dir_all(ws.join("incoming")).unwrap();
    std::fs::write(
        ws.join("incoming/.wargame.json"),
        r#"{"provider":"dreamhack","challenge_id":"11","title":"t","download_sha256":{}}"#,
    )
    .unwrap();

    // A `python3` on PATH that always fails, as a macOS image without the
    // Command Line Tools has.
    let fake_bin = tmp.path().join("bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let fake_python = fake_bin.join("python3");
    std::fs::write(&fake_python, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_python, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let path = format!(
        "{}:{}",
        fake_bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = std::process::Command::new(bash())
        .env("PATH", path)
        .arg(guest_dir().join("bootstrap.sh"))
        .arg(&ws)
        .arg("--extract=false")
        .output()
        .expect("failed to spawn bash");
    assert!(
        out.status.success(),
        "a broken python3 must not fail the bootstrap: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let readme = std::fs::read_to_string(ws.join("notes/README.md")).unwrap();
    assert!(readme.contains("dreamhack"), "{readme}");
}
