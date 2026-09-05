//! `CredentialStore` implementations that persist to disk (DESIGN.md §3.3,
//! §3.6): the macOS Keychain via the `security` CLI, and a plaintext JSON
//! fallback for tests/CI.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use secrecy::ExposeSecret;
use tracing::warn;

use crate::error::{Result, WargameError};
use crate::infra::process::{Cmd, Output, ProcessRunner};
use crate::ports::{CredentialStore, Secret};

const SERVICE: &str = "wargame-vm";

/// Quotes `s` as a single token for `security -i` stdin scripts.
///
/// `security -i` is *not* a POSIX shell: it has no close/reopen escaping
/// inside single quotes (`'a'\''b'` is rejected as a usage error) and it
/// drops a bare backslash inside single quotes (`'back\slash'` becomes
/// `backslash`), so [`crate::infra::ssh::shell_quote`] is the wrong tool
/// here even though it happens to work for values that contain neither `'`
/// nor `\` (e.g. the JSON session payload this store actually persists,
/// which is alphanumeric/`{}[]:,"` only). This instead always produces a
/// double-quoted token with `\` and `"` backslash-escaped, which `security
/// -i` accepts for arbitrary bytes.
fn security_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Runs `fut` to completion regardless of whether we're already inside a
/// tokio runtime, by handing it to a dedicated worker thread with its own
/// current-thread runtime. `CredentialStore` is a synchronous trait (secrets
/// must be readable from plain, non-async call sites like config wiring)
/// while [`ProcessRunner`] is async, so this is the bridge between the two.
fn block_on<F>(fut: F) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build a worker runtime")
            .block_on(fut)
    })
    .join()
    .expect("keychain worker thread panicked")
}

fn run_blocking(runner: &Arc<dyn ProcessRunner>, cmd: Cmd) -> Result<Output> {
    let runner = runner.clone();
    block_on(async move { runner.run(&cmd).await })
}

/// The macOS Keychain, accessed through the `security` CLI. Secrets are
/// always sent through stdin (`security -i`), never as an argv element.
pub struct KeychainStore {
    runner: Arc<dyn ProcessRunner>,
    service: String,
}

impl KeychainStore {
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        KeychainStore {
            runner,
            service: SERVICE.to_string(),
        }
    }
}

impl CredentialStore for KeychainStore {
    fn get(&self, key: &str) -> Result<Option<Secret<String>>> {
        let cmd = Cmd::new(
            "security",
            vec![
                "find-generic-password".to_string(),
                "-a".to_string(),
                key.to_string(),
                "-s".to_string(),
                self.service.clone(),
                "-w".to_string(),
            ],
        );
        let out = run_blocking(&self.runner, cmd)?;
        if out.status != 0 {
            return Ok(None);
        }
        let value = out.stdout.trim_end_matches(['\n', '\r']).to_string();
        Ok(Some(Secret::new(Box::new(value))))
    }

    fn set(&self, key: &str, value: &Secret<String>) -> Result<()> {
        let script = format!(
            "add-generic-password -U -a {} -s {} -w {}\n",
            security_quote(key),
            security_quote(&self.service),
            security_quote(value.expose_secret()),
        );
        let cmd = Cmd {
            program: "security".to_string(),
            args: vec!["-i".to_string()],
            env: Vec::new(),
            stdin: Some(script.into_bytes()),
            cwd: None,
        };
        let out = run_blocking(&self.runner, cmd)?;
        if out.status != 0 {
            return Err(WargameError::CredentialStore(format!(
                "security -i add-generic-password failed: {}",
                out.stderr
            )));
        }
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        let script = format!(
            "delete-generic-password -a {} -s {}\n",
            security_quote(key),
            security_quote(&self.service),
        );
        let cmd = Cmd {
            program: "security".to_string(),
            args: vec!["-i".to_string()],
            env: Vec::new(),
            stdin: Some(script.into_bytes()),
            cwd: None,
        };
        let out = run_blocking(&self.runner, cmd)?;
        if out.status != 0 {
            return Err(WargameError::CredentialStore(format!(
                "security -i delete-generic-password failed: {}",
                out.stderr
            )));
        }
        Ok(())
    }
}

/// A plaintext JSON credential store (0600). Tests/CI only — using it in
/// production warns loudly at construction time.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        warn!(path = %path.display(), "using a plaintext credential store; the macOS Keychain is safer");
        FileStore { path }
    }

    fn read_map(&self) -> Result<BTreeMap<String, String>> {
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let content = std::fs::read_to_string(&self.path)?;
        serde_json::from_str(&content).map_err(|e| WargameError::CredentialStore(e.to_string()))
    }

    fn write_map(&self, map: &BTreeMap<String, String>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(map)
            .map_err(|e| WargameError::CredentialStore(e.to_string()))?;
        std::fs::write(&self.path, content)?;
        set_mode_0600(&self.path)?;
        Ok(())
    }
}

#[cfg(unix)]
fn set_mode_0600(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode_0600(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

impl CredentialStore for FileStore {
    fn get(&self, key: &str) -> Result<Option<Secret<String>>> {
        Ok(self
            .read_map()?
            .get(key)
            .map(|v| Secret::new(Box::new(v.clone()))))
    }

    fn set(&self, key: &str, value: &Secret<String>) -> Result<()> {
        let mut map = self.read_map()?;
        map.insert(key.to_string(), value.expose_secret().to_string());
        self.write_map(&map)
    }

    fn delete(&self, key: &str) -> Result<()> {
        let mut map = self.read_map()?;
        map.remove(key);
        self.write_map(&map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeRunner;

    #[test]
    fn keychain_get_uses_find_generic_password_argv_only() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(|_| {
            Ok(Output {
                status: 0,
                stdout: "s3cr3t\n".to_string(),
                stderr: String::new(),
            })
        });
        let store = KeychainStore::new(runner.clone());

        let value = store.get("dreamhack.session").unwrap().unwrap();
        assert_eq!(value.expose_secret(), "s3cr3t");

        let calls = runner.calls();
        assert_eq!(calls[0].program, "security");
        assert_eq!(calls[0].args[0], "find-generic-password");
        assert!(calls[0].args.contains(&"dreamhack.session".to_string()));
        assert!(calls[0].stdin.is_none());
    }

    #[test]
    fn keychain_get_missing_returns_none() {
        let runner = Arc::new(FakeRunner::new());
        runner.set_responder(|_| {
            Ok(Output {
                status: 44,
                stdout: String::new(),
                stderr: "not found".to_string(),
            })
        });
        let store = KeychainStore::new(runner);
        assert!(store.get("missing").unwrap().is_none());
    }

    #[test]
    fn keychain_set_sends_secret_through_stdin_never_argv() {
        let runner = Arc::new(FakeRunner::new());
        let store = KeychainStore::new(runner.clone());

        let secret = Secret::new(Box::new("super-secret-cookie".to_string()));
        store.set("dreamhack.session", &secret).unwrap();

        let calls = runner.calls();
        assert_eq!(calls.len(), 1);
        let cmd = &calls[0];
        assert_eq!(cmd.program, "security");
        assert_eq!(cmd.args, vec!["-i".to_string()]);
        // The secret must never show up in argv.
        for arg in &cmd.args {
            assert!(!arg.contains("super-secret-cookie"));
        }
        let stdin = String::from_utf8(cmd.stdin.clone().unwrap()).unwrap();
        assert!(stdin.contains("super-secret-cookie"));
        assert!(stdin.starts_with("add-generic-password -U -a"));
    }

    #[test]
    fn keychain_delete_uses_stdin_script() {
        let runner = Arc::new(FakeRunner::new());
        let store = KeychainStore::new(runner.clone());
        store.delete("dreamhack.session").unwrap();

        let calls = runner.calls();
        assert_eq!(calls[0].args, vec!["-i".to_string()]);
        let stdin = String::from_utf8(calls[0].stdin.clone().unwrap()).unwrap();
        assert!(stdin.starts_with("delete-generic-password -a"));
    }

    #[test]
    fn security_quote_escapes_backslash_and_double_quote() {
        assert_eq!(security_quote("plain"), "\"plain\"");
        assert_eq!(
            security_quote(r#"back\slash"#),
            r#""back\\slash""#,
            "a bare backslash must survive, unlike security -i's own single-quote handling"
        );
        assert_eq!(security_quote(r#"a"b"#), r#""a\"b""#);
        // The DESIGN §3.6 payload: a JSON blob containing double quotes.
        let json = r#"{"cookies":[{"name":"sessionid","value":"abc"}]}"#;
        assert_eq!(
            security_quote(json),
            format!("\"{}\"", json.replace('\\', "\\\\").replace('"', "\\\""))
        );
        // A single quote (the character that breaks security -i's own
        // close/reopen idiom) needs no special handling in a double-quoted
        // token.
        assert_eq!(security_quote("a'b"), "\"a'b\"");
    }

    #[test]
    fn keychain_set_quotes_secret_for_security_i_not_a_posix_shell() {
        let runner = Arc::new(FakeRunner::new());
        let store = KeychainStore::new(runner.clone());

        // A value containing a single quote and a backslash: `shell_quote`
        // (POSIX shell escaping) would mis-round-trip this through
        // `security -i`, which is not a POSIX shell.
        let secret = Secret::new(Box::new(r#"a'b\c"d"#.to_string()));
        store.set("dreamhack.session", &secret).unwrap();

        let calls = runner.calls();
        let stdin = String::from_utf8(calls[0].stdin.clone().unwrap()).unwrap();
        assert!(stdin.contains(&security_quote(r#"a'b\c"d"#)));
    }

    #[test]
    fn file_store_round_trips_and_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let store = FileStore::new(path.clone());

        assert!(store.get("k").unwrap().is_none());
        let secret = Secret::new(Box::new("value".to_string()));
        store.set("k", &secret).unwrap();
        assert_eq!(store.get("k").unwrap().unwrap().expose_secret(), "value");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        store.delete("k").unwrap();
        assert!(store.get("k").unwrap().is_none());
    }
}
