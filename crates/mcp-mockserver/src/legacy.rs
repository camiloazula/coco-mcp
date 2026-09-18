//! A transport that answers `server/discover` itself, the way a server that
//! only knows the `initialize` handshake does.
//!
//! Hand-written because refusing discovery inside the handler is not enough
//! over stdio: once `rmcp`'s server has seen a request other than
//! `initialize` first, it keeps requiring per-request metadata, and so turns
//! away the handshake client that falls back after the refusal. Answering
//! before the server sees the request leaves it a plain legacy server.

use rmcp::RoleServer;
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, DiscoverRequestMethod, ErrorData, ServerJsonRpcMessage,
};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;

/// Wraps a server transport and answers every `server/discover` with
/// method-not-found without passing it on.
#[derive(Debug)]
pub struct RefuseDiscover<T> {
    inner: T,
}

impl<T> RefuseDiscover<T> {
    /// Wrap `inner`.
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T: Transport<RoleServer>> Transport<RoleServer> for RefuseDiscover<T> {
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            let message = self.inner.receive().await?;
            let ClientJsonRpcMessage::Request(request) = &message else {
                return Some(message);
            };
            if !matches!(request.request, ClientRequest::DiscoverRequest(_)) {
                return Some(message);
            }
            let refusal = ServerJsonRpcMessage::error(
                ErrorData::method_not_found::<DiscoverRequestMethod>(),
                Some(request.id.clone()),
            );
            if self.inner.send(refusal).await.is_err() {
                return None;
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}
