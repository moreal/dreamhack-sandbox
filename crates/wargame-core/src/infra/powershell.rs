//! PowerShell command encoding for Windows guests (spec.md §3 FR-4:
//! "게스트 접속은 우선 SSH/SFTP로 통일하되 Windows 명령은 PowerShell에 맞게
//! 인코딩한다", "Unix shell quoting을 Windows에 재사용하지 않는다").
//!
//! Two layers, both of which keep host-side shell strings out of the picture
//! entirely (spec.md §5 item 6):
//!
//! * [`quote`] turns one argument into a PowerShell *single-quoted literal*,
//!   where the only metacharacter is `'` (doubled). `$`, backticks,
//!   backslashes, double quotes, spaces and non-ASCII text are all literal
//!   inside such a string, so a Windows path like
//!   `C:\Users\사용자\wargame\it's "here"` survives unchanged.
//! * [`PowerShellCommand`] wraps argv-like data in the call operator and
//!   hands the whole script to `powershell -EncodedCommand`, i.e. UTF-16LE +
//!   base64. Nothing about the command then depends on how `ssh`, `sshd` or
//!   the guest's default shell would have re-split it: newlines, quotes and
//!   Korean characters cross the wire as base64.
//!
//! The base64 codec is implemented here rather than pulled in as a
//! dependency: it is 30 lines, it is exercised by the round-trip tests
//! below, and `-EncodedCommand` is the only place the project needs one.

/// Quotes `s` as a PowerShell single-quoted string literal.
///
/// Only `'` is special inside such a literal, and it is escaped by doubling.
/// This is deliberately *not* [`crate::infra::ssh::shell_quote`]: reusing
/// POSIX's `'\''` escape would leave a stray backslash in the guest's
/// argument.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// UTF-16LE + base64, the encoding `powershell -EncodedCommand` expects.
pub fn encode_command(script: &str) -> String {
    let mut utf16le = Vec::with_capacity(script.len() * 2);
    for unit in script.encode_utf16() {
        utf16le.extend_from_slice(&unit.to_le_bytes());
    }
    base64_encode(&utf16le)
}

/// The inverse of [`encode_command`], for diagnostics and tests. Returns
/// `None` if `encoded` is not base64 of valid UTF-16LE.
pub fn decode_command(encoded: &str) -> Option<String> {
    let bytes = base64_decode(encoded)?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

/// A command to run in a Windows guest, built from argv-like data.
///
/// The generated script calls the argv through the call operator (`&`) with
/// every element as a literal, and ends with `exit $LASTEXITCODE` so the
/// guest command's own exit code reaches the host -- which is what lets
/// [`crate::app::profile_build::REBOOT_REQUIRED_EXIT_CODE`] (3010) be
/// recognised rather than swallowed by `powershell.exe`'s own status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerShellCommand {
    script: String,
}

impl PowerShellCommand {
    /// Wraps `argv` -- `argv[0]` is the command, the rest are its arguments.
    /// An empty argv produces a script that succeeds and does nothing.
    pub fn from_argv(argv: &[String]) -> Self {
        if argv.is_empty() {
            return PowerShellCommand {
                script: "exit 0\n".to_string(),
            };
        }
        let call = argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
        PowerShellCommand::from_script(format!("& {call}"))
    }

    /// Wraps a trusted script body in the same preamble/epilogue. The body
    /// must be a template this crate owns; any *data* in it belongs in
    /// [`quote`]d literals.
    pub fn from_script(body: impl AsRef<str>) -> Self {
        // `$ErrorActionPreference = 'Stop'` turns a cmdlet's non-terminating
        // error into a terminating one so it cannot be mistaken for success;
        // a *native* command's non-zero exit is not an error in PowerShell,
        // so it still arrives through `$LASTEXITCODE`.
        let script = format!(
            "$ErrorActionPreference = 'Stop'\n\
             $ProgressPreference = 'SilentlyContinue'\n\
             $global:LASTEXITCODE = 0\n\
             try {{\n\
             {}\n\
             }} catch {{\n\
             [Console]::Error.WriteLine($_.Exception.Message)\n\
             exit 1\n\
             }}\n\
             exit $LASTEXITCODE\n",
            body.as_ref()
        );
        PowerShellCommand { script }
    }

    /// The script text the guest runs.
    pub fn script(&self) -> &str {
        &self.script
    }

    /// The script as `-EncodedCommand` expects it.
    pub fn encoded(&self) -> String {
        encode_command(&self.script)
    }

    /// The remote argv to hand to `ssh`, i.e. everything after `user@host`.
    pub fn remote_argv(&self) -> Vec<String> {
        vec![
            "powershell".to_string(),
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-EncodedCommand".to_string(),
            self.encoded(),
        ]
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(triple >> 18) as usize & 0x3f] as char);
        out.push(B64[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            B64[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(encoded: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let bytes = encoded.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().filter(|c| **c == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut triple = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            let v = if *c == b'=' { 0 } else { value(*c)? };
            triple |= v << (18 - 6 * i);
        }
        out.push((triple >> 16) as u8);
        if pad < 2 {
            out.push((triple >> 8) as u8);
        }
        if pad < 1 {
            out.push(triple as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_quotes_are_doubled_not_backslash_escaped() {
        assert_eq!(quote("it's"), "'it''s'");
        assert_eq!(quote("plain"), "'plain'");
        assert!(!quote("it's").contains('\\'));
    }

    #[test]
    fn every_awkward_character_stays_literal() {
        for value in [
            r"C:\wargame",
            "$env:PATH",
            "back`tick",
            r#"say "hi""#,
            "with spaces",
            "한글",
        ] {
            assert_eq!(quote(value), format!("'{value}'"));
        }
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        for encoded in ["", "Zg==", "Zm8=", "Zm9v", "Zm9vYg==", "Zm9vYmFy"] {
            assert_eq!(
                base64_encode(&base64_decode(encoded).unwrap()),
                encoded,
                "round trip failed for {encoded:?}"
            );
        }
        assert!(base64_decode("Zg=").is_none(), "unpadded input is rejected");
        assert!(base64_decode("Z!==").is_none(), "non-base64 is rejected");
    }

    /// `Get-Content` on a UTF-16LE file starts with the BOM-less byte pairs
    /// PowerShell expects; the well-known vector below is `powershell
    /// -EncodedCommand`'s own encoding of `Write-Host 'hi'`.
    #[test]
    fn encode_command_is_utf16le_base64() {
        assert_eq!(encode_command("hi"), "aABpAA==");
        assert_eq!(decode_command("aABpAA==").as_deref(), Some("hi"));
        assert_eq!(decode_command("****").as_deref(), None);
        // A lone surrogate is not valid UTF-16.
        assert_eq!(decode_command(&base64_encode(&[0x00, 0xd8])), None);
    }

    #[test]
    fn korean_and_newlines_survive_the_round_trip() {
        let script = "Write-Output '한글 경로'\nWrite-Output 'second line'";
        assert_eq!(
            decode_command(&encode_command(script)).as_deref(),
            Some(script)
        );
    }

    #[test]
    fn an_empty_argv_is_a_no_op() {
        let command = PowerShellCommand::from_argv(&[]);
        assert_eq!(command.script(), "exit 0\n");
    }

    #[test]
    fn from_argv_calls_the_command_with_literal_arguments() {
        let command = PowerShellCommand::from_argv(&[
            "New-Item".to_string(),
            "-Path".to_string(),
            r"C:\wargame\it's here".to_string(),
        ]);
        assert!(command
            .script()
            .contains(r"& 'New-Item' '-Path' 'C:\wargame\it''s here'"));
        assert!(command.script().ends_with("exit $LASTEXITCODE\n"));
    }
}
