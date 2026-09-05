//! An in-memory [`ProcessRunner`] that records every [`Cmd`] it was asked to
//! run and answers from a configurable queue/responder, so tests can assert
//! on exact argv (e.g. "the secret was never in argv") without touching a
//! real subprocess.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::Result;
use crate::infra::process::{Cmd, Output, ProcessRunner};

type Responder = dyn Fn(&Cmd) -> Result<Output> + Send + Sync;

pub struct FakeRunner {
    calls: Mutex<Vec<Cmd>>,
    responses: Mutex<Vec<Output>>,
    responder: Mutex<Option<Box<Responder>>>,
    default_output: Output,
    next_pid: Mutex<u32>,
    which_map: Mutex<BTreeMap<String, PathBuf>>,
    alive_pids: Mutex<BTreeSet<u32>>,
}

impl FakeRunner {
    pub fn new() -> Self {
        FakeRunner {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(Vec::new()),
            responder: Mutex::new(None),
            default_output: Output {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            next_pid: Mutex::new(1000),
            which_map: Mutex::new(BTreeMap::new()),
            alive_pids: Mutex::new(BTreeSet::new()),
        }
    }

    /// Every `Cmd` passed to `run`/`spawn_detached` so far, in order.
    pub fn calls(&self) -> Vec<Cmd> {
        self.calls.lock().unwrap().clone()
    }

    /// Queues an `Output` for the next `run()` call (FIFO); once the queue
    /// is empty, `run()` falls back to `Output { status: 0, .. }`.
    pub fn push_response(&self, out: Output) {
        self.responses.lock().unwrap().push(out);
    }

    /// Queues a successful (`status: 0`) response with the given stdout.
    pub fn push_success(&self, stdout: impl Into<String>) {
        self.push_response(Output {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        });
    }

    /// Queues a failing response with the given status and stderr.
    pub fn push_failure(&self, status: i32, stderr: impl Into<String>) {
        self.push_response(Output {
            status,
            stdout: String::new(),
            stderr: stderr.into(),
        });
    }

    /// Installs a function that computes the response for every `run()`
    /// call from its `Cmd`. Overrides the queue set by [`Self::push_response`].
    pub fn set_responder(&self, f: impl Fn(&Cmd) -> Result<Output> + Send + Sync + 'static) {
        *self.responder.lock().unwrap() = Some(Box::new(f));
    }

    /// Makes `which(program)` resolve to `path`.
    pub fn set_which(&self, program: &str, path: PathBuf) {
        self.which_map
            .lock()
            .unwrap()
            .insert(program.to_string(), path);
    }

    /// Marks a pid returned by `spawn_detached` as no longer alive.
    pub fn mark_dead(&self, pid: u32) {
        self.alive_pids.lock().unwrap().remove(&pid);
    }
}

impl Default for FakeRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ProcessRunner for FakeRunner {
    async fn run(&self, cmd: &Cmd) -> Result<Output> {
        self.calls.lock().unwrap().push(cmd.clone());
        if let Some(f) = self.responder.lock().unwrap().as_ref() {
            return f(cmd);
        }
        let mut responses = self.responses.lock().unwrap();
        if !responses.is_empty() {
            return Ok(responses.remove(0));
        }
        Ok(self.default_output.clone())
    }

    async fn spawn_detached(&self, cmd: &Cmd, _log_path: &Path) -> Result<u32> {
        self.calls.lock().unwrap().push(cmd.clone());
        let pid = {
            let mut next = self.next_pid.lock().unwrap();
            let pid = *next;
            *next += 1;
            pid
        };
        self.alive_pids.lock().unwrap().insert(pid);
        Ok(pid)
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.which_map.lock().unwrap().get(program).cloned()
    }

    fn pid_alive(&self, pid: u32) -> bool {
        self.alive_pids.lock().unwrap().contains(&pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_calls_and_answers_from_queue() {
        let runner = FakeRunner::new();
        runner.push_response(Output {
            status: 1,
            stdout: "a".to_string(),
            stderr: String::new(),
        });
        runner.push_response(Output {
            status: 2,
            stdout: "b".to_string(),
            stderr: String::new(),
        });

        let cmd = Cmd::new("tart", vec!["list".to_string()]);
        let first = runner.run(&cmd).await.unwrap();
        let second = runner.run(&cmd).await.unwrap();
        let third = runner.run(&cmd).await.unwrap();

        assert_eq!(first.status, 1);
        assert_eq!(second.status, 2);
        assert_eq!(third.status, 0); // default once queue drains
        assert_eq!(runner.calls().len(), 3);
    }

    #[tokio::test]
    async fn spawn_detached_returns_distinct_alive_pids() {
        let runner = FakeRunner::new();
        let cmd = Cmd::new("tart", vec!["run".to_string()]);
        let pid1 = runner
            .spawn_detached(&cmd, Path::new("/tmp/log"))
            .await
            .unwrap();
        let pid2 = runner
            .spawn_detached(&cmd, Path::new("/tmp/log"))
            .await
            .unwrap();

        assert_ne!(pid1, pid2);
        assert!(runner.pid_alive(pid1));
        runner.mark_dead(pid1);
        assert!(!runner.pid_alive(pid1));
        assert!(runner.pid_alive(pid2));
    }

    #[test]
    fn which_uses_configured_map() {
        let runner = FakeRunner::new();
        assert!(runner.which("tart").is_none());
        runner.set_which("tart", PathBuf::from("/usr/local/bin/tart"));
        assert_eq!(
            runner.which("tart"),
            Some(PathBuf::from("/usr/local/bin/tart"))
        );
    }

    #[tokio::test]
    async fn push_success_and_failure_helpers() {
        let runner = FakeRunner::new();
        runner.push_success("ok");
        runner.push_failure(1, "bad");

        let cmd = Cmd::new("tart", vec!["list".to_string()]);
        let first = runner.run(&cmd).await.unwrap();
        let second = runner.run(&cmd).await.unwrap();

        assert_eq!(first.status, 0);
        assert_eq!(first.stdout, "ok");
        assert_eq!(second.status, 1);
        assert_eq!(second.stderr, "bad");
    }
}
