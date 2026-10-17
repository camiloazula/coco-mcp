//! A stdio server: a process spoken to in lines of JSON.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt as _;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

use crate::upstream::{
    Connector, Frame, INCOMING, MAX_MESSAGE, Opened, RequestContext, StderrSink, Upstream,
    UpstreamError,
};

/// Longest stderr line kept; the rest of a longer one is dropped.
const STDERR_LINE: usize = 64 * 1024;

/// Starts the server as a process of its own for each agent.
#[derive(Debug, Clone)]
pub struct StdioConnector {
    /// The program.
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Environment added to the gateway's own.
    pub env: BTreeMap<String, String>,
    /// Where it runs.
    pub cwd: Option<PathBuf>,
}

#[async_trait]
impl Connector for StdioConnector {
    async fn open(&self, stderr: StderrSink) -> Result<Opened, UpstreamError> {
        let mut command = tokio::process::Command::new(&self.command);
        command
            .args(&self.args)
            .envs(&self.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
        let mut child = command.spawn().map_err(|e| {
            UpstreamError::Failed(format!("failed to start `{}`: {e}", self.command))
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(UpstreamError::Failed("the process has no pipes".into()));
        };
        if let Some(pipe) = child.stderr.take() {
            tokio::spawn(forward_stderr(pipe, stderr));
        }
        Ok(stdio_over_streams(stdout, stdin, Some(child)))
    }
}

/// A server spoken to over a pair of byte streams: a process's pipes, or,
/// in a test, the two ends of a duplex. `child` is stopped with the session.
pub fn stdio_over_streams<R, W>(
    reader: R,
    writer: W,
    child: Option<tokio::process::Child>,
) -> Opened
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let (tx, incoming) = mpsc::channel(INCOMING);
    let reading = tokio::spawn(read_messages(reader, tx)).abort_handle();
    let upstream = Process {
        writer: tokio::sync::Mutex::new(Box::pin(writer)),
        child: Mutex::new(child),
        reading,
    };
    Opened {
        upstream: Arc::new(upstream),
        incoming,
    }
}

type Writer = Pin<Box<dyn AsyncWrite + Send>>;

struct Process {
    writer: tokio::sync::Mutex<Writer>,
    child: Mutex<Option<tokio::process::Child>>,
    reading: tokio::task::AbortHandle,
}

impl Process {
    fn stop(&self) {
        self.reading.abort();
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.start_kill();
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

#[async_trait]
impl Upstream for Process {
    async fn send(&self, frame: &Frame, _context: &RequestContext) -> Result<(), UpstreamError> {
        // One message to a line: a message the agent pretty-printed is
        // written compact.
        let mut line = serde_json::to_vec(&frame.value)
            .map_err(|e| UpstreamError::Failed(format!("cannot write the message: {e}")))?;
        line.push(b'\n');
        let mut writer = self.writer.lock().await;
        writer
            .write_all(&line)
            .await
            .map_err(|_| UpstreamError::Closed)?;
        writer.flush().await.map_err(|_| UpstreamError::Closed)
    }

    async fn close(&self) {
        self.stop();
        let mut writer = self.writer.lock().await;
        let _ = writer.shutdown().await;
    }
}

/// The messages a server writes, one to a line, until it stops.
async fn read_messages<R: AsyncRead + Unpin>(reader: R, tx: mpsc::Sender<Value>) {
    let mut lines = FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_MESSAGE));
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(line) {
                    Ok(message) => {
                        if tx.send(message).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => tracing::warn!("a stdio server wrote a line that is not JSON: {e}"),
                }
            }
            Err(LinesCodecError::MaxLineLengthExceeded) => {
                tracing::warn!("a stdio server wrote a message over {MAX_MESSAGE} bytes; dropped");
            }
            Err(LinesCodecError::Io(_)) => break,
        }
    }
}

/// A process's stderr, a line at a time, to `sink`.
async fn forward_stderr(pipe: tokio::process::ChildStderr, sink: StderrSink) {
    let mut lines = FramedRead::new(pipe, LinesCodec::new_with_max_length(STDERR_LINE));
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => sink(line),
            Err(LinesCodecError::MaxLineLengthExceeded) => {}
            Err(LinesCodecError::Io(_)) => break,
        }
    }
}
