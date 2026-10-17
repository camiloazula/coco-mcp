//! Where the messages for an agent go: the stream of each request it made,
//! the stream it opened for what the server says on its own, and a few
//! messages kept for one that is not open yet.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc;

/// Messages one stream can have waiting.
pub(crate) const STREAM: usize = 256;
/// Messages kept for a stream that is not open yet; the oldest go first.
const BACKLOG: usize = 256;

#[derive(Default)]
struct State {
    /// Requests waiting for their response, by id.
    waiting: HashMap<String, mpsc::Sender<Value>>,
    /// The ids in `waiting`, oldest first.
    order: Vec<String>,
    /// The stream the agent opened for what the server says on its own.
    listener: Option<mpsc::Sender<Value>>,
    backlog: VecDeque<Value>,
    last_active: Option<Instant>,
}

/// The streams of one agent connection.
#[derive(Default)]
pub(crate) struct Streams {
    state: Mutex<State>,
}

impl Streams {
    pub(crate) fn new() -> Self {
        let streams = Self::default();
        streams.touch();
        streams
    }

    /// Note that the agent did something.
    pub(crate) fn touch(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.last_active = Some(Instant::now());
        }
    }

    /// Whether the agent has done nothing for `limit`, has no stream open and
    /// waits for nothing: a connection nobody is using.
    pub(crate) fn is_idle(&self, limit: Duration) -> bool {
        self.state.lock().is_ok_and(|state| {
            let listening = state.listener.as_ref().is_some_and(|l| !l.is_closed());
            let quiet = state.last_active.is_none_or(|at| at.elapsed() >= limit);
            quiet && !listening && state.waiting.is_empty()
        })
    }

    /// A request is waiting for its response under `key`; its messages come
    /// out of the receiver, which ends when the response has been sent.
    pub(crate) fn register(&self, key: &str) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::channel(STREAM);
        if let Ok(mut state) = self.state.lock() {
            state.waiting.insert(key.to_owned(), tx);
            state.order.retain(|k| k != key);
            state.order.push(key.to_owned());
        }
        rx
    }

    /// Hand `message`, the response, to the request waiting under `key`.
    /// `false` when nothing waits there.
    pub(crate) async fn answer(&self, key: &str, message: Value) -> bool {
        let target = self.state.lock().ok().and_then(|mut state| {
            state.order.retain(|k| k != key);
            state.waiting.remove(key)
        });
        match target {
            Some(target) => {
                let _ = target.send(message).await;
                true
            }
            None => false,
        }
    }

    /// Place a message that answers nothing: on the agent's stream, else on
    /// the latest request still waiting, else keep it for a stream.
    pub(crate) async fn unsolicited(&self, message: Value) {
        let target = self.state.lock().ok().and_then(|state| {
            state
                .listener
                .clone()
                .filter(|l| !l.is_closed())
                .or_else(|| {
                    state
                        .order
                        .last()
                        .and_then(|key| state.waiting.get(key).cloned())
                })
        });
        let kept = match target {
            Some(target) => target.send(message).await.err().map(|e| e.0),
            None => Some(message),
        };
        if let Some(message) = kept
            && let Ok(mut state) = self.state.lock()
        {
            if state.backlog.len() >= BACKLOG {
                state.backlog.pop_front();
            }
            state.backlog.push_back(message);
        }
    }

    /// Open the agent's stream. `None` when one is open already.
    pub(crate) fn listen(&self) -> Option<mpsc::Receiver<Value>> {
        self.touch();
        let (tx, rx) = mpsc::channel(STREAM);
        let mut state = self.state.lock().ok()?;
        if state.listener.as_ref().is_some_and(|l| !l.is_closed()) {
            return None;
        }
        while let Some(message) = state.backlog.pop_front() {
            if tx.try_send(message).is_err() {
                break;
            }
        }
        state.listener = Some(tx);
        Some(rx)
    }

    /// End every stream.
    pub(crate) fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.waiting.clear();
            state.order.clear();
            state.listener = None;
        }
    }
}
