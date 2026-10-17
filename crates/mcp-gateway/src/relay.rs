//! One agent's connection to one server: what it posts goes to the server,
//! and what the server says comes back to the right place.
//!
//! A response goes to the request it answers. What the server says on its
//! own (a notification, a request for sampling) is not an answer to
//! anything: it goes to the stream the agent opened for it, else to the
//! latest request still waiting, else it is kept until a stream opens.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::Direction;
use crate::frame::{Kind, id_key, kind};
use crate::observe::{Observer, Traffic};
use crate::upstream::{Frame, INCOMING, Opened, RequestContext, Upstream, UpstreamError};

/// Messages kept for a stream that is not open yet; the oldest go first.
const BACKLOG: usize = 256;

/// What posting a message came to.
#[derive(Debug)]
pub(crate) enum Posted {
    /// Nothing is owed back: a notification, or an answer to the server.
    Accepted,
    /// A request: the messages for it, ending with its response.
    Stream(mpsc::Receiver<Value>),
}

/// A request sent and not yet answered.
struct Waiting {
    tx: mpsc::Sender<Value>,
    method: String,
    at: Instant,
}

#[derive(Default)]
struct State {
    /// Requests waiting for their response, by id.
    waiting: HashMap<String, Waiting>,
    /// The ids in `waiting`, oldest first.
    order: Vec<String>,
    /// The stream the agent opened for what the server says on its own.
    listener: Option<mpsc::Sender<Value>>,
    /// What the server said with nowhere to put it.
    backlog: VecDeque<Value>,
    last_active: Option<Instant>,
}

/// An agent's connection.
pub(crate) struct Relay {
    pub(crate) id: String,
    pub(crate) namespace: String,
    upstream: Arc<dyn Upstream>,
    observer: Arc<dyn Observer>,
    state: Mutex<State>,
    ended: CancellationToken,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("namespace", &self.namespace)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Relay {
    /// Start relaying over `opened`.
    pub(crate) fn start(
        namespace: String,
        id: String,
        opened: Opened,
        observer: Arc<dyn Observer>,
    ) -> Arc<Self> {
        let Opened { upstream, incoming } = opened;
        let relay = Arc::new(Self {
            id,
            namespace,
            upstream,
            observer,
            state: Mutex::new(State {
                last_active: Some(Instant::now()),
                ..State::default()
            }),
            ended: CancellationToken::new(),
        });
        tokio::spawn(relay.clone().pump(incoming));
        relay
    }

    /// Whether the connection is over.
    pub(crate) fn is_closed(&self) -> bool {
        self.ended.is_cancelled()
    }

    /// Whether the agent has done nothing here for `limit` and has no stream
    /// open: a connection nobody is using.
    pub(crate) fn is_idle(&self, limit: Duration) -> bool {
        self.state.lock().is_ok_and(|state| {
            let listening = state.listener.as_ref().is_some_and(|l| !l.is_closed());
            let quiet = state.last_active.is_none_or(|at| at.elapsed() >= limit);
            // A request whose agent stopped listening (it cancelled, or
            // left) is not work anyone waits for.
            let waited_on = state.waiting.values().any(|w| !w.tx.is_closed());
            quiet && !listening && !waited_on
        })
    }

    fn touch(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.last_active = Some(Instant::now());
        }
    }

    /// Send what the agent posted on to the server.
    pub(crate) async fn post(
        &self,
        frame: Frame,
        context: &RequestContext,
    ) -> Result<Posted, UpstreamError> {
        if self.is_closed() {
            return Err(UpstreamError::Closed);
        }
        self.touch();
        let waiting = match kind(&frame.value) {
            Kind::Request(id) => {
                let key = id_key(&id);
                let (tx, rx) = mpsc::channel(INCOMING);
                let method = frame.value["method"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                if let Ok(mut state) = self.state.lock() {
                    state.waiting.insert(
                        key.clone(),
                        Waiting {
                            tx,
                            method,
                            at: Instant::now(),
                        },
                    );
                    state.order.retain(|k| *k != key);
                    state.order.push(key.clone());
                }
                Some((key, rx))
            }
            _ => None,
        };
        self.observer.frame(
            &self.namespace,
            &self.id,
            Traffic {
                direction: Direction::Outbound,
                frame: &frame.value,
                method: None,
                elapsed: None,
            },
        );
        if let Err(e) = self.upstream.send(&frame, context).await {
            if let Some((key, _)) = &waiting {
                self.forget(key);
            }
            return Err(e);
        }
        Ok(match waiting {
            Some((_, rx)) => Posted::Stream(rx),
            None => Posted::Accepted,
        })
    }

    fn forget(&self, key: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.waiting.remove(key);
            state.order.retain(|k| k != key);
        }
    }

    /// Open the stream for what the server says on its own. `None` when one
    /// is open already.
    pub(crate) fn listen(&self) -> Option<mpsc::Receiver<Value>> {
        self.touch();
        let (tx, rx) = mpsc::channel(INCOMING);
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

    /// Read what the server says until it stops, and give each message its
    /// place.
    async fn pump(self: Arc<Self>, mut incoming: mpsc::Receiver<Value>) {
        loop {
            let message = tokio::select! {
                () = self.ended.cancelled() => return,
                message = incoming.recv() => message,
            };
            let Some(message) = message else {
                break;
            };
            self.route(message).await;
        }
        self.close("the server ended the session").await;
    }

    async fn route(&self, message: Value) {
        let mut answered = None;
        if let Kind::Response(id) = kind(&message) {
            let key = id_key(&id);
            answered = self.state.lock().ok().and_then(|mut state| {
                state.order.retain(|k| *k != key);
                state.waiting.remove(&key)
            });
        }
        self.observer.frame(
            &self.namespace,
            &self.id,
            Traffic {
                direction: Direction::Inbound,
                frame: &message,
                method: answered.as_ref().map(|w| w.method.as_str()),
                elapsed: answered.as_ref().map(|w| w.at.elapsed()),
            },
        );
        if let Some(waiting) = answered {
            let _ = waiting.tx.send(message).await;
            return;
        }
        // Not an answer to anything waiting: a notification, a request to
        // the agent, or a response nobody is waiting for.
        let target = self.state.lock().ok().and_then(|state| {
            state
                .listener
                .clone()
                .filter(|l| !l.is_closed())
                .or_else(|| {
                    state
                        .order
                        .last()
                        .and_then(|key| state.waiting.get(key).map(|w| w.tx.clone()))
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

    /// End the connection: the server's session, every stream.
    pub(crate) async fn close(&self, reason: &str) {
        if self.ended.is_cancelled() {
            return;
        }
        self.ended.cancel();
        self.upstream.close().await;
        if let Ok(mut state) = self.state.lock() {
            state.waiting.clear();
            state.order.clear();
            state.listener = None;
        }
        self.observer.closed(&self.namespace, &self.id, reason);
    }
}
