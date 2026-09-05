//! An in-memory [`GuestTransport`] that records every `exec`/`copy_in` call
//! and can be configured to fail specific steps or refuse readiness.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::{Result, WargameError};
use crate::ports::guest_transport::{ExecResult, GuestTransport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportCall {
    Exec {
        ip: IpAddr,
        argv: Vec<String>,
    },
    CopyIn {
        ip: IpAddr,
        src: PathBuf,
        dest: String,
    },
}

/// A per-argv responder: given the exact argv passed to `exec`, returns the
/// `ExecResult` to answer with. Consulted before the blanket
/// `exec_status`/`fail_on` behavior so tests can script specific commands
/// (e.g. a verify command's stderr) while leaving everything else on the
/// simple defaults.
pub type ExecResponder = dyn Fn(&[String]) -> ExecResult + Send + Sync;

pub struct FakeTransport {
    pub ready: Mutex<bool>,
    calls: Mutex<Vec<TransportCall>>,
    fail_on: Mutex<Option<&'static str>>,
    exec_status: Mutex<i32>,
    exec_responder: Mutex<Option<Box<ExecResponder>>>,
}

impl FakeTransport {
    pub fn new() -> Self {
        FakeTransport {
            ready: Mutex::new(true),
            calls: Mutex::new(Vec::new()),
            fail_on: Mutex::new(None),
            exec_status: Mutex::new(0),
            exec_responder: Mutex::new(None),
        }
    }

    pub fn calls(&self) -> Vec<TransportCall> {
        self.calls.lock().unwrap().clone()
    }

    pub fn set_ready(&self, ready: bool) {
        *self.ready.lock().unwrap() = ready;
    }

    /// Steps: `"wait_ready"`, `"exec"`, `"copy_in"`.
    pub fn set_fail_on(&self, step: &'static str) {
        *self.fail_on.lock().unwrap() = Some(step);
    }

    pub fn set_exec_status(&self, status: i32) {
        *self.exec_status.lock().unwrap() = status;
    }

    /// Installs a per-argv responder for `exec`, so different commands (e.g.
    /// a specific provisioning script vs. a specific verify command) can be
    /// made to return different statuses/stdout/stderr in one test.
    pub fn set_exec_responder<F>(&self, responder: F)
    where
        F: Fn(&[String]) -> ExecResult + Send + Sync + 'static,
    {
        *self.exec_responder.lock().unwrap() = Some(Box::new(responder));
    }

    fn should_fail(&self, step: &str) -> bool {
        self.fail_on
            .lock()
            .unwrap()
            .map(|s| s == step)
            .unwrap_or(false)
    }
}

impl Default for FakeTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl GuestTransport for FakeTransport {
    async fn wait_ready(&self, _ip: IpAddr, timeout: Duration) -> Result<()> {
        if self.should_fail("wait_ready") || !*self.ready.lock().unwrap() {
            return Err(WargameError::GuestBootTimeout(timeout));
        }
        Ok(())
    }

    async fn exec(&self, ip: IpAddr, argv: &[String]) -> Result<ExecResult> {
        self.calls.lock().unwrap().push(TransportCall::Exec {
            ip,
            argv: argv.to_vec(),
        });
        if self.should_fail("exec") {
            return Err(WargameError::GuestBootstrapFailed(
                "fake exec failure".to_string(),
            ));
        }
        if let Some(responder) = self.exec_responder.lock().unwrap().as_ref() {
            return Ok(responder(argv));
        }
        Ok(ExecResult {
            status: *self.exec_status.lock().unwrap(),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    async fn copy_in(&self, ip: IpAddr, src: &Path, dest: &str) -> Result<()> {
        self.calls.lock().unwrap().push(TransportCall::CopyIn {
            ip,
            src: src.to_path_buf(),
            dest: dest.to_string(),
        });
        if self.should_fail("copy_in") {
            return Err(WargameError::CopyFailed("fake copy failure".to_string()));
        }
        Ok(())
    }

    fn interactive_shell_cmd(&self, ip: IpAddr) -> Command {
        let mut cmd = Command::new("true");
        cmd.arg(ip.to_string());
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[tokio::test]
    async fn records_exec_and_copy_in_calls() {
        let transport = FakeTransport::new();
        let ip = IpAddr::from_str("10.0.0.5").unwrap();

        transport
            .exec(ip, &["echo".to_string(), "hi".to_string()])
            .await
            .unwrap();
        transport
            .copy_in(ip, Path::new("/tmp/a.zip"), "~/wargame/incoming/")
            .await
            .unwrap();

        let calls = transport.calls();
        assert_eq!(calls.len(), 2);
        assert!(matches!(&calls[0], TransportCall::Exec { .. }));
        assert!(matches!(&calls[1], TransportCall::CopyIn { .. }));
    }

    #[tokio::test]
    async fn set_fail_on_makes_the_named_step_error() {
        let transport = FakeTransport::new();
        transport.set_fail_on("exec");
        let ip = IpAddr::from_str("10.0.0.5").unwrap();
        let err = transport.exec(ip, &["true".to_string()]).await.unwrap_err();
        assert!(matches!(err, WargameError::GuestBootstrapFailed(_)));
    }

    #[tokio::test]
    async fn not_ready_times_out() {
        let transport = FakeTransport::new();
        transport.set_ready(false);
        let ip = IpAddr::from_str("10.0.0.5").unwrap();
        let err = transport
            .wait_ready(ip, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(err, WargameError::GuestBootTimeout(_)));
    }
}
