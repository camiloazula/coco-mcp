//! The `subscriptions/listen` stream of a 2026-07-28 session.
//!
//! A modern server sends list changes and resource updates only on a stream
//! the client opened for them. A session keeps one, for the lists the server
//! says change and the resources subscribed to; subscribing or unsubscribing
//! opens it again with the new set. Its notifications become the same
//! [`EventKind::ListChanged`] and [`EventKind::ResourceUpdated`] events a
//! legacy server's notifications do. `rmcp` routes them to the stream alone,
//! not to the client handler, so nothing is reported twice.

use std::collections::BTreeSet;
use std::time::Duration;

use rmcp::model::{ServerNotification, SubscriptionFilter};
use rmcp::service::SubscriptionEnd;
use rmcp::{Peer, RoleClient};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::event::{EventKind, EventSink, ListKind};
use crate::{Error, Result};

/// How long an abruptly ended stream waits before it opens again, doubled
/// after each failure up to [`LAST_RETRY`].
const FIRST_RETRY: Duration = Duration::from_millis(250);
const LAST_RETRY: Duration = Duration::from_secs(30);

/// The topic of the notes the stream leaves in the log.
const TOPIC: &str = "subscriptions/listen";

/// The lists whose changes a server with `capabilities` announces.
pub(crate) fn list_filter(capabilities: &Value) -> SubscriptionFilter {
    let announces = |list: &str| {
        capabilities
            .get(list)
            .and_then(|c| c.get("listChanged"))
            .and_then(Value::as_bool)
            == Some(true)
    };
    let mut filter = SubscriptionFilter::builder();
    if announces("tools") {
        filter = filter.tools_list_changed();
    }
    if announces("prompts") {
        filter = filter.prompts_list_changed();
    }
    if announces("resources") {
        filter = filter.resources_list_changed();
    }
    filter.build()
}

#[derive(Debug)]
enum Change {
    Subscribe(String),
    Unsubscribe(String),
}

type Reply = oneshot::Sender<Result<()>>;

/// The task keeping a session's stream open, and the way to change what it
/// listens for.
#[derive(Debug)]
pub(crate) struct Listener {
    changes: mpsc::UnboundedSender<(Change, Reply)>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Listener {
    /// Listen on `peer` for `lists` and, later, the resources subscribed to,
    /// until `cancel`.
    pub(crate) fn spawn(
        peer: Peer<RoleClient>,
        lists: SubscriptionFilter,
        sink: EventSink,
        cancel: CancellationToken,
    ) -> Self {
        let (changes, received) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(peer, lists, sink, received, cancel));
        Self { changes, task }
    }

    /// Add `uri` and wait until the server accepted it on the reopened
    /// stream, or did not.
    pub(crate) async fn subscribe(&self, uri: &str) -> Result<()> {
        self.change(Change::Subscribe(uri.to_owned())).await
    }

    /// Leave `uri` out of the stream.
    pub(crate) async fn unsubscribe(&self, uri: &str) -> Result<()> {
        self.change(Change::Unsubscribe(uri.to_owned())).await
    }

    async fn change(&self, change: Change) -> Result<()> {
        let (reply, answer) = oneshot::channel();
        self.changes
            .send((change, reply))
            .map_err(|_| Error::Closed)?;
        answer.await.map_err(|_| Error::Closed)?
    }
}

/// How reading a stream ended.
enum Ended {
    /// What it listens for changed: open it again now.
    Changed,
    /// It broke: open it again after a wait.
    Broke,
    /// The server closed it: wait for a change before opening another.
    Closed,
    /// The session is over.
    Stop,
}

async fn run(
    peer: Peer<RoleClient>,
    lists: SubscriptionFilter,
    sink: EventSink,
    mut changes: mpsc::UnboundedReceiver<(Change, Reply)>,
    cancel: CancellationToken,
) {
    let mut uris = BTreeSet::new();
    let mut waiting: Vec<(Change, Reply)> = Vec::new();
    let mut retry = FIRST_RETRY;
    loop {
        let wanted = filter(&lists, &uris);
        if wanted == SubscriptionFilter::default() {
            // Nothing to listen for, so no stream.
            settle(&mut uris, &mut waiting, None);
            if !idle(&mut changes, &mut uris, &mut waiting, &cancel).await {
                return;
            }
            continue;
        }
        let opened = tokio::select! {
            opened = peer.listen(wanted.clone()) => opened,
            () = cancel.cancelled() => return,
        };
        let mut stream = match opened {
            Ok(stream) => stream,
            Err(e) => {
                let text = format!("the stream could not be opened: {e}");
                note(&sink, &text, true);
                settle(&mut uris, &mut waiting, Some(&text));
                if !pause(retry, &mut changes, &mut uris, &mut waiting, &cancel).await {
                    return;
                }
                retry = (retry * 2).min(LAST_RETRY);
                continue;
            }
        };
        let accepted = stream.acknowledged().clone();
        note_declined(&sink, &wanted, &accepted);
        settle_accepted(&mut uris, &mut waiting, &accepted);
        let ended = loop {
            tokio::select! {
                next = stream.next() => match next {
                    Ok(Some(notification)) => {
                        retry = FIRST_RETRY;
                        emit(&sink, notification);
                    }
                    Ok(None) => match stream.end() {
                        Some(SubscriptionEnd::Graceful(_) | SubscriptionEnd::Cancelled) => {
                            note(&sink, "the server closed the stream", false);
                            break Ended::Closed;
                        }
                        _ => {
                            note(&sink, "the stream ended abruptly; opening it again", true);
                            break Ended::Broke;
                        }
                    },
                    Err(e) => {
                        note(&sink, &format!("the stream failed: {e}; opening it again"), true);
                        break Ended::Broke;
                    }
                },
                change = changes.recv() => match change {
                    Some(change) => {
                        apply(&mut uris, change, &mut waiting);
                        let _ = stream.cancel().await;
                        break Ended::Changed;
                    }
                    None => {
                        let _ = stream.cancel().await;
                        break Ended::Stop;
                    }
                },
                () = cancel.cancelled() => {
                    let _ = stream.cancel().await;
                    break Ended::Stop;
                }
            }
        };
        drop(stream);
        let go_on = match ended {
            Ended::Changed => true,
            Ended::Broke => {
                let waited = pause(retry, &mut changes, &mut uris, &mut waiting, &cancel).await;
                retry = (retry * 2).min(LAST_RETRY);
                waited
            }
            Ended::Closed => idle(&mut changes, &mut uris, &mut waiting, &cancel).await,
            Ended::Stop => false,
        };
        if !go_on {
            return;
        }
    }
}

/// `lists` plus a subscription to each of `uris`.
fn filter(lists: &SubscriptionFilter, uris: &BTreeSet<String>) -> SubscriptionFilter {
    let mut filter = lists.clone();
    if !uris.is_empty() {
        filter.resource_subscriptions = Some(uris.iter().cloned().collect());
    }
    filter
}

fn apply(
    uris: &mut BTreeSet<String>,
    (change, reply): (Change, Reply),
    waiting: &mut Vec<(Change, Reply)>,
) {
    match &change {
        Change::Subscribe(uri) => {
            uris.insert(uri.clone());
        }
        Change::Unsubscribe(uri) => {
            uris.remove(uri);
        }
    }
    waiting.push((change, reply));
}

/// Answer the changes waiting for a stream that did not open (`failure`), or
/// that is not needed.
fn settle(uris: &mut BTreeSet<String>, waiting: &mut Vec<(Change, Reply)>, failure: Option<&str>) {
    for (change, reply) in waiting.drain(..) {
        let answer = match (change, failure) {
            (Change::Subscribe(uri), Some(failure)) => {
                uris.remove(&uri);
                Err(Error::Refused(failure.to_owned()))
            }
            _ => Ok(()),
        };
        let _ = reply.send(answer);
    }
}

/// Answer the changes waiting for a stream the server acknowledged with
/// `accepted`: a subscription it left out is refused and forgotten.
fn settle_accepted(
    uris: &mut BTreeSet<String>,
    waiting: &mut Vec<(Change, Reply)>,
    accepted: &SubscriptionFilter,
) {
    for (change, reply) in waiting.drain(..) {
        let answer = match change {
            Change::Subscribe(uri)
                if !accepted
                    .resource_subscriptions
                    .as_ref()
                    .is_some_and(|kept| kept.contains(&uri)) =>
            {
                uris.remove(&uri);
                Err(Error::Refused(format!(
                    "the server did not accept a subscription to {uri}"
                )))
            }
            _ => Ok(()),
        };
        let _ = reply.send(answer);
    }
}

/// Wait for a change, then apply it. `false` when the session is over.
async fn idle(
    changes: &mut mpsc::UnboundedReceiver<(Change, Reply)>,
    uris: &mut BTreeSet<String>,
    waiting: &mut Vec<(Change, Reply)>,
    cancel: &CancellationToken,
) -> bool {
    tokio::select! {
        change = changes.recv() => match change {
            Some(change) => {
                apply(uris, change, waiting);
                true
            }
            None => false,
        },
        () = cancel.cancelled() => false,
    }
}

/// Wait `delay`, cut short by a change, which is applied. `false` when the
/// session is over.
async fn pause(
    delay: Duration,
    changes: &mut mpsc::UnboundedReceiver<(Change, Reply)>,
    uris: &mut BTreeSet<String>,
    waiting: &mut Vec<(Change, Reply)>,
    cancel: &CancellationToken,
) -> bool {
    tokio::select! {
        () = tokio::time::sleep(delay) => true,
        change = changes.recv() => match change {
            Some(change) => {
                apply(uris, change, waiting);
                true
            }
            None => false,
        },
        () = cancel.cancelled() => false,
    }
}

/// Say in the log what the server left out of what was asked for.
fn note_declined(sink: &EventSink, wanted: &SubscriptionFilter, accepted: &SubscriptionFilter) {
    let mut declined = Vec::new();
    for (name, asked, kept) in [
        (
            "tool list changes",
            wanted.tools_list_changed,
            accepted.tools_list_changed,
        ),
        (
            "prompt list changes",
            wanted.prompts_list_changed,
            accepted.prompts_list_changed,
        ),
        (
            "resource list changes",
            wanted.resources_list_changed,
            accepted.resources_list_changed,
        ),
    ] {
        if asked == Some(true) && kept != Some(true) {
            declined.push(name.to_owned());
        }
    }
    let kept = accepted.resource_subscriptions.clone().unwrap_or_default();
    declined.extend(
        wanted
            .resource_subscriptions
            .iter()
            .flatten()
            .filter(|uri| !kept.contains(uri))
            .cloned(),
    );
    if !declined.is_empty() {
        let text = format!("the server did not accept {}", declined.join(", "));
        note(sink, &text, false);
    }
}

fn emit(sink: &EventSink, notification: ServerNotification) {
    let kind = match notification {
        ServerNotification::ToolListChangedNotification(_) => {
            EventKind::ListChanged(ListKind::Tools)
        }
        ServerNotification::PromptListChangedNotification(_) => {
            EventKind::ListChanged(ListKind::Prompts)
        }
        ServerNotification::ResourceListChangedNotification(_) => {
            EventKind::ListChanged(ListKind::Resources)
        }
        ServerNotification::ResourceUpdatedNotification(update) => EventKind::ResourceUpdated {
            uri: update.params.uri,
        },
        _ => return,
    };
    sink.emit(kind);
}

fn note(sink: &EventSink, text: &str, is_error: bool) {
    sink.emit(EventKind::Note {
        topic: TOPIC.to_owned(),
        text: text.to_owned(),
        is_error,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_stream_listens_only_for_the_lists_the_server_announces() {
        let filter = list_filter(&json!({
            "tools": {"listChanged": true},
            "prompts": {},
            "resources": {"subscribe": true, "listChanged": true}
        }));
        assert_eq!(filter.tools_list_changed, Some(true));
        assert_eq!(filter.prompts_list_changed, None);
        assert_eq!(filter.resources_list_changed, Some(true));
        assert_eq!(list_filter(&Value::Null), SubscriptionFilter::default());
    }

    #[test]
    fn a_subscription_the_server_left_out_is_refused_and_forgotten() {
        let mut uris = BTreeSet::from(["a".to_owned(), "b".to_owned()]);
        let (kept_reply, kept) = oneshot::channel();
        let (left_reply, left) = oneshot::channel();
        let mut waiting = vec![
            (Change::Subscribe("a".into()), kept_reply),
            (Change::Subscribe("b".into()), left_reply),
        ];
        let accepted = SubscriptionFilter::builder()
            .resource_subscription("a")
            .build();
        settle_accepted(&mut uris, &mut waiting, &accepted);
        assert!(matches!(kept.blocking_recv(), Ok(Ok(()))));
        assert!(matches!(left.blocking_recv(), Ok(Err(Error::Refused(_)))));
        assert_eq!(uris, BTreeSet::from(["a".to_owned()]));
        assert!(waiting.is_empty());
    }

    #[test]
    fn subscriptions_are_added_to_the_lists() {
        let lists = SubscriptionFilter::builder().tools_list_changed().build();
        assert_eq!(filter(&lists, &BTreeSet::new()), lists);
        let with = filter(&lists, &BTreeSet::from(["mock://counter".to_owned()]));
        assert_eq!(
            with.resource_subscriptions.as_deref(),
            Some(&["mock://counter".to_owned()][..])
        );
        assert_eq!(with.tools_list_changed, Some(true));
    }
}
