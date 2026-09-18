//! Answers to server-initiated requests: building the dialog's entities
//! and turning what the user typed into a [`ServerResponse`].

use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::{AppContext as _, Context, Entity, Subscription, Window};
use mcp_core::{ElicitationAction, ElicitationMode, ServerRequestKind, ServerResponse};
use serde_json::{Value, json};

use crate::views::Workspace;
use crate::views::form::ToolForm;
use crate::views::request_dialog::REQUEST_TREE;
use crate::views::server_view::parse_roots;

/// Entities backing the dialog for one request.
pub struct RequestUi {
    /// `(server id, request id)` the entities were built for.
    pub(super) key: (String, Value),
    pub(super) form: Option<Entity<ToolForm>>,
    pub(super) model: Entity<InputState>,
    pub(super) text: Entity<InputState>,
    pub(super) roots: Entity<TextareaState>,
    /// Enter in the sampling reply answers, as the Reply button does.
    _enter: Vec<Subscription>,
}

impl std::fmt::Debug for RequestUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestUi").finish_non_exhaustive()
    }
}

impl Workspace {
    /// Rebuild the dialog entities when the request at the head changes.
    pub fn sync_request_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let head = self.state.read(cx).current_request().map(|p| {
            (
                p.server_id.clone(),
                p.request.id.clone(),
                p.request.kind.clone(),
            )
        });
        let Some((server_id, id, kind)) = head else {
            self.request_ui = None;
            return;
        };
        if self
            .request_ui
            .as_ref()
            .is_some_and(|ui| ui.key == (server_id.clone(), id.clone()))
        {
            return;
        }
        let form = match &kind {
            ServerRequestKind::Elicitation {
                mode: ElicitationMode::Form { schema },
                ..
            } => Some(cx.new(|cx| ToolForm::new(schema, window, cx))),
            _ => None,
        };
        // A roots request starts from the roots offered to this server before.
        let roots = self
            .state
            .read(cx)
            .servers
            .iter()
            .find(|s| s.record.id == server_id)
            .and_then(|s| s.roots.as_ref())
            .map(|roots| {
                roots
                    .iter()
                    .map(|root| root.uri.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Model")
                .default_value("coco-mcp")
        });
        let text = cx.new(|cx| InputState::new(window, cx).placeholder("Assistant reply"));
        let enter = [&model, &text]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.accept_request(cx);
                    }
                })
            })
            .collect();
        let ui = RequestUi {
            key: (server_id, id),
            form,
            model,
            text,
            _enter: enter,
            roots: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("file:///path/to/project")
                    .default_value(roots)
            }),
        };
        // The dialog's request tree starts folded: it is there so nothing is
        // hidden, not to push the answer fields off screen.
        self.collapsed.insert(format!("{REQUEST_TREE}$"));
        self.request_ui = Some(ui);
    }

    /// Answer the current request with the primary action.
    pub fn accept_request(&mut self, cx: &mut Context<Self>) {
        let Some(ui) = &self.request_ui else {
            return;
        };
        let kind = self
            .state
            .read(cx)
            .current_request()
            .map(|p| p.request.kind.clone());
        let response = match kind {
            Some(ServerRequestKind::Elicitation { mode, .. }) => match mode {
                ElicitationMode::Form { .. } => {
                    let Some(form) = ui.form.clone() else {
                        return;
                    };
                    let Some(content) = form.update(cx, |f, cx| f.validated_json(cx)) else {
                        cx.notify();
                        return;
                    };
                    ServerResponse::Elicitation {
                        action: ElicitationAction::Accept,
                        content: Some(content),
                    }
                }
                ElicitationMode::Url { url, .. } => {
                    // The handler admits only http(s); checked again at the
                    // one place that hands a server's URL to the system.
                    let lower = url.to_ascii_lowercase();
                    if lower.starts_with("https://") || lower.starts_with("http://") {
                        let _ = open::that_detached(&url);
                    }
                    ServerResponse::Elicitation {
                        action: ElicitationAction::Accept,
                        content: None,
                    }
                }
            },
            Some(ServerRequestKind::Sampling(_)) => {
                let model = ui.model.read(cx).value().to_string();
                let text = ui.text.read(cx).value().to_string();
                ServerResponse::Sampling(json!({
                    "role": "assistant",
                    "content": {"type": "text", "text": text},
                    "model": if model.is_empty() { "coco-mcp".to_owned() } else { model },
                    "stopReason": "endTurn"
                }))
            }
            Some(ServerRequestKind::ListRoots) => {
                ServerResponse::Roots(parse_roots(&ui.roots.read(cx).value()))
            }
            None => return,
        };
        self.state
            .update(cx, |s, cx| s.answer_request(response, cx));
    }

    /// Answer the current request negatively (`Decline` / `Reject`).
    pub fn decline_request(&mut self, cx: &mut Context<Self>) {
        let kind = self
            .state
            .read(cx)
            .current_request()
            .map(|p| p.request.kind.clone());
        let response = match kind {
            Some(ServerRequestKind::Elicitation { .. }) => ServerResponse::Elicitation {
                action: ElicitationAction::Decline,
                content: None,
            },
            Some(ServerRequestKind::Sampling(_)) => ServerResponse::Reject {
                message: "sampling rejected by the user".into(),
            },
            Some(ServerRequestKind::ListRoots) => ServerResponse::Roots(Vec::new()),
            None => return,
        };
        self.state
            .update(cx, |s, cx| s.answer_request(response, cx));
    }

    /// Dismiss the current request (`Cancel` for elicitations, reject otherwise).
    pub fn cancel_request(&mut self, cx: &mut Context<Self>) {
        let kind = self
            .state
            .read(cx)
            .current_request()
            .map(|p| p.request.kind.clone());
        let response = match kind {
            Some(ServerRequestKind::Elicitation { .. }) => ServerResponse::Elicitation {
                action: ElicitationAction::Cancel,
                content: None,
            },
            Some(_) => ServerResponse::Reject {
                message: "request dismissed by the user".into(),
            },
            None => return,
        };
        self.state
            .update(cx, |s, cx| s.answer_request(response, cx));
    }
}
