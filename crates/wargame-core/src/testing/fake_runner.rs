//! [`FakeRunner`]: an in-memory [`ProcessRunner`] that records every [`Cmd`]
//! it is given and answers from a configurable responder or FIFO queue of
//! canned [`Output`]s, defaulting to a successful empty one. Lets tests
//! assert exact argv (and, crucially, that secrets travel through `stdin`/
//! `env` rather than `args`) without spawning real processes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::{Result, WargameError};
use crate::infra::process::{Cmd, Output, ProcessRunner};

type Responder = Box<dyn Fn(&Cmd) -> Result<Output> + Send + Sync>;

pub struct FakeRunner {
    /// Every `Cmd` passed to `run` or `spawn_detached`, in call order.
    pub calls: Mutex<Vec<Cmd>>,
    responder: Mutex<Option<Responder>>,
    queue: Mutex<VecDeque<Result<Output>>>,
    which: Mutex<HashMap<String, PathBuf>>,
    alive_pids: Mutex<HashSet<u32>>,
    next_pid: Mutex<u32>,
    /// When set, any call whose `program` equals this fails immediately.
    fail_on: Mutex<Option<&'static str>>,
}

impl Default for FakeRunner {
    fn default() -> Self {
        FakeRunner {
            calls: Mutex::new(Vec::new()),
            responder: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            which: Mutex::new(HashMap::new()),
            alive_pids: Mutex::new(HashSet::new()),
            next_pid: Mutex::new(1000),
            fail_on: Mutex::new(None),
        }
    }
}

impl FakeRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues one successful/failing answer, consumed FIFO by calls that
    /// aren't intercepted by a `responder`.
    pub fn push_output(&self, output: Output) {
        self.queue.lock().unwrap().push_back(Ok(output));
    }

    pub fn push_success(&self, stdout: impl Into<String>) {
        self.push_output(Output {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        });
    }

    pub fn push_failure(&self, status: i32, stderr: impl Into<String>) {
        self.push_output(Output {
            status,
            stdout: String::new(),
            stderr: stderr.into(),
        });
    }

    /// Installs a responder computing the answer for every call; overrides
    /// the queue while it's set.
    pub fn set_responder<F>(&self, f: F)
    where
        F: Fn(&Cmd) -> Result<Output> + Send + Sync + 'static,
    {
        *self.responder.lock().unwrap() = Some(Box::new(f));
    }

    /// Makes every call whose program is `program` return an `Err`.
    pub fn fail_on(&self, program: &'static str) {
        *self.fail_on.lock().unwrap() = Some(program);
    }

    pub fn set_which(&self, program: &str, path: PathBuf) {
        self.which.lock().unwrap().insert(program.to_string(), path);
    }

    pub fn set_pid_alive(&self, pid: u32, alive: bool) {
        let mut set = self.alive_pids.lock().unwrap();
        if alive {
            set.insert(pid);
        } else {
            set.remove(&pid);
        }
    }

    /// A snapshot of every call recorded so far, in order.
    pub fn calls(&self) -> Vec<Cmd> {
        self.calls.lock().unwrap().clone()
    }

    fn answer(&self, cmd: &Cmd) -> Result<Output> {
        if let Some(program) = *self.fail_on.lock().unwrap() {
            if cmd.program == program {
                return Err(WargameError::Other(format!(
                    "FakeRunner: forced failure for {program}"
                )));
            }
        }
        if let Some(responder) = self.responder.lock().unwrap().as_ref() {
            return responder(cmd);
        }
        if let Some(next) = self.queue.lock().unwrap().pop_front() {
            return next;
        }
        Ok(Output::default())
    }
}

#[async_trait]
impl ProcessRunner for FakeRunner {
    async fn run(&self, cmd: &Cmd) -> Result<Output> {
        self.calls.lock().unwrap().push(cmd.clone());
        self.answer(cmd)
    }

    async fn spawn_detached(&self, cmd: &Cmd, _log_path: &Path) -> Result<u32> {
        self.calls.lock().unwrap().push(cmd.clone());
        self.answer(cmd)?;
        let mut next = self.next_pid.lock().unwrap();
        let pid = *next;
        *next += 1;
        self.alive_pids.lock().unwrap().insert(pid);
        Ok(pid)
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.which.lock().unwrap().get(program).cloned()
    }

    fn pid_alive(&self, pid: u32) -> bool {
        self.alive_pids.lock().unwrap().contains(&pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_calls_and_defaults_to_success() {
        let runner = FakeRunner::new();
        let out = runner.run(&Cmd::new("tart", ["list"])).await.unwrap();
        assert_eq!(out.status, 0);
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(runner.calls()[0].args, vec!["list".to_string()]);
    }

    #[tokio::test]
    async fn stdin_is_recorded_but_never_touches_args() {
        let runner = FakeRunner::new();
        runner
            .run(&Cmd::new("security", ["add-generic-password"]).with_stdin(b"s3cr3t".to_vec()))
            .await
            .unwrap();
        let call = &runner.calls()[0];
        assert_eq!(call.stdin.as_deref(), Some(&b"s3cr3t"[..]));
        assert!(!call.args.iter().any(|a| a.contains("s3cr3t")));
    }

    #[tokio::test]
    async fn queue_is_consumed_fifo() {
        let runner = FakeRunner::new();
        runner.push_failure(1, "not ready");
        runner.push_failure(1, "not ready");
        runner.push_success("10.0.0.5\n");
        let cmd = Cmd::new("tart", ["ip", "wg-x"]);
        assert_eq!(runner.run(&cmd).await.unwrap().status, 1);
        assert_eq!(runner.run(&cmd).await.unwrap().status, 1);
        assert_eq!(runner.run(&cmd).await.unwrap().stdout, "10.0.0.5\n");
    }

    #[tokio::test]
    async fn responder_overrides_queue() {
        let runner = FakeRunner::new();
        runner.set_responder(|cmd| {
            if cmd.args.first().map(String::as_str) == Some("set") {
                Ok(Output {
                    status: 1,
                    stdout: String::new(),
                    stderr: "boom".to_string(),
                })
            } else {
                Ok(Output::default())
            }
        });
        assert_eq!(
            runner
                .run(&Cmd::new("tart", ["clone"]))
                .await
                .unwrap()
                .status,
            0
        );
        assert_eq!(
            runner.run(&Cmd::new("tart", ["set"])).await.unwrap().status,
            1
        );
    }

    #[tokio::test]
    async fn spawn_detached_returns_increasing_pids_marked_alive() {
        let runner = FakeRunner::new();
        let p1 = runner
            .spawn_detached(&Cmd::new("tart", ["run"]), Path::new("/tmp/x.log"))
            .await
            .unwrap();
        let p2 = runner
            .spawn_detached(&Cmd::new("tart", ["run"]), Path::new("/tmp/x.log"))
            .await
            .unwrap();
        assert_ne!(p1, p2);
        assert!(runner.pid_alive(p1));
        assert!(runner.pid_alive(p2));
    }

    #[tokio::test]
    async fn fail_on_forces_an_error_for_matching_program() {
        let runner = FakeRunner::new();
        runner.fail_on("tart");
        assert!(runner.run(&Cmd::new("tart", ["list"])).await.is_err());
    }
}
