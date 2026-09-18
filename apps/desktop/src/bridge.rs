//! The tokio ↔ GPUI bridge.
//!
//! `mcp-core` runs on a multi-threaded tokio runtime living on its own
//! thread. Every `Session` call is a tokio future, so the UI never awaits one
//! directly: it hands the future to [`Bridge::run`], which spawns it on the
//! runtime and returns a GPUI-awaitable future backed by a oneshot channel.
//! Events flow the other way, turned on the runtime into what the UI applies,
//! through an unbounded `futures` channel polled on the foreground executor.
//! Nothing here touches an `Entity`.

use std::future::Future;
use std::sync::Arc;

use futures::channel::{mpsc, oneshot};
use mcp_core::{Event, EventSink};

/// Handle to the background runtime. Cheap to clone.
#[derive(Clone)]
pub struct Bridge {
    runtime: Arc<tokio::runtime::Runtime>,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bridge")
    }
}

impl Bridge {
    /// Start the runtime.
    pub fn new() -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("mcp-core")
            .build()?;
        Ok(Self {
            runtime: Arc::new(runtime),
        })
    }

    /// Run `fut` on the tokio runtime; the returned future resolves on
    /// whichever executor awaits it. Resolves to `None` if the task panicked.
    pub fn run<F>(&self, fut: F) -> impl Future<Output = Option<F::Output>> + Send + 'static
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.runtime.spawn(async move {
            let _ = tx.send(fut.await);
        });
        async move { rx.await.ok() }
    }

    /// Run blocking work (SQLite) on the runtime's blocking pool.
    pub fn run_blocking<F, R>(&self, f: F) -> impl Future<Output = Option<R>> + Send + 'static
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.runtime.spawn_blocking(move || {
            let _ = tx.send(f());
        });
        async move { rx.await.ok() }
    }

    /// Forward every event from `sink`, as `map` turns it, into a channel the
    /// UI can poll. `map` runs on the runtime, so reading a large message
    /// never holds up a frame. Stops when the sink is dropped or the receiver
    /// goes away.
    pub fn forward_events<T: Send + 'static>(
        &self,
        sink: &EventSink,
        map: impl Fn(Event) -> T + Send + 'static,
    ) -> mpsc::UnboundedReceiver<T> {
        let (tx, rx) = mpsc::unbounded();
        let mut events = sink.subscribe();
        self.runtime.spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        if tx.unbounded_send(map(event)).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        rx
    }
}

/// `first` and every message already queued behind it, in order, without
/// waiting for more. A receiver that wakes on a burst files all of it at once.
pub fn drain_ready<T>(first: T, rx: &mut mpsc::UnboundedReceiver<T>) -> Vec<T> {
    let mut batch = vec![first];
    // Stops on an empty queue and on a closed one alike.
    while let Ok(next) = rx.try_recv() {
        batch.push(next);
    }
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_is_drained_in_one_batch() {
        let (tx, mut rx) = mpsc::unbounded();
        for i in 1..=50 {
            tx.unbounded_send(i).unwrap();
        }
        let batch = drain_ready(0, &mut rx);
        assert_eq!(batch, (0..=50).collect::<Vec<_>>(), "all 51, in order");
        assert_eq!(drain_ready(7, &mut rx), [7], "nothing else was queued");
        tx.unbounded_send(8).unwrap();
        drop(tx);
        assert_eq!(drain_ready(7, &mut rx), [7, 8], "a closed channel ends it");
        assert_eq!(drain_ready(9, &mut rx), [9]);
    }

    #[test]
    fn run_returns_the_value() {
        let bridge = Bridge::new().unwrap();
        let value = futures::executor::block_on(bridge.run(async { 40 + 2 }));
        assert_eq!(value, Some(42));
        let blocking = futures::executor::block_on(bridge.run_blocking(|| "ok"));
        assert_eq!(blocking, Some("ok"));
    }

    #[test]
    fn events_are_forwarded() {
        use futures::StreamExt as _;
        let bridge = Bridge::new().unwrap();
        let sink = EventSink::new(16);
        let mut rx = bridge.forward_events(&sink, |event| event.kind.summary());
        std::thread::sleep(std::time::Duration::from_millis(20));
        sink.emit(mcp_core::EventKind::Stderr { line: "hi".into() });
        let summary = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(summary, "stderr: hi", "the event arrives as `map` made it");
    }
}
