//! The Gateway section: whether it runs, where it listens, the access token,
//! and which servers it answers for under which paths.

use super::*;
use gpui_kit::SharedString;

use crate::gateway::{self, Status};

impl Settings {
    /// The port input, with its error when the last entry was refused.
    fn port_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let field = h_flex()
            .gap(px(10.))
            .items_center()
            .child(
                div()
                    .w(px(90.))
                    .child(Input::new(&self.port).id("gateway-port").h(px(26.))),
            )
            .child(muted(cx, 11., "Press Enter to apply."));
        let error = self
            .port_error
            .clone()
            .map(|e| div().text_size(px(11.)).text_color(t.err).child(e));
        self.row_with(
            "Port",
            v_flex()
                .gap(px(4.))
                .child(field)
                .children(error)
                .into_any_element(),
            cx,
        )
    }

    /// The access token: its end, and what can be done with it.
    fn token_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let hint = self.state.read(cx).gateway.token_hint();
        let shown = hint
            .clone()
            .unwrap_or_else(|| "made when the gateway first starts".into());
        let mut controls = h_flex().gap(px(16.)).items_center().child(
            div()
                .font_family(crate::theme::mono(cx))
                .text_size(px(12.))
                .text_color(if hint.is_some() { t.fg } else { t.muted })
                .child(shown),
        );
        if hint.is_some() {
            controls = controls
                .child(
                    self.choice("token-copy", "Copy", false, true, cx, |this, _, cx| {
                        let token = this.state.read(cx).gateway.token().map(str::to_owned);
                        if let Some(token) = token {
                            crate::clip::copy(token, "the gateway's access token", cx);
                        }
                    }),
                )
                .child(self.choice(
                    "token-regenerate",
                    "Regenerate",
                    false,
                    true,
                    cx,
                    |this, _, cx| {
                        this.state
                            .update(cx, |s, cx| s.gateway_regenerate_token(cx))
                    },
                ));
        }
        self.row_with(
            "Access token",
            v_flex()
                .gap(px(4.))
                .child(controls)
                .child(muted(
                    cx,
                    11.,
                    "An agent sends it as a bearer token. Regenerating turns away the agents using the old one.",
                ).whitespace_normal())
                .into_any_element(),
            cx,
        )
    }

    /// The address that is every included server at once, and the config for it.
    fn root_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let (url, any) = {
            let state = self.state.read(cx);
            let records: Vec<_> = state.servers.iter().map(|s| s.record.clone()).collect();
            let any = state
                .gateway
                .settings
                .plan(&records)
                .iter()
                .any(|(_, p)| p.on);
            (
                format!("http://127.0.0.1:{}/", state.gateway.settings.port),
                any,
            )
        };
        let copied = url.clone();
        let content = v_flex()
            .gap(px(4.))
            .child(
                h_flex()
                    .gap(px(12.))
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::theme::mono(cx))
                            .text_size(px(11.))
                            .text_color(if any { t.fg } else { t.muted })
                            .child(url),
                    )
                    .child(self.choice(
                        "copy-config-root",
                        "Copy config",
                        false,
                        any,
                        cx,
                        move |_, _, cx| {
                            let entry = ("coco-mcp".to_owned(), copied.clone());
                            let config = mcp_exchange::gateway_config(&[entry]);
                            crate::clip::copy(config.text(), "agent config · token left out", cx);
                        },
                    )),
            )
            .child(
                muted(
                    cx,
                    11.,
                    "Every server below as one: its tools and prompts are named <server>__<name>.",
                )
                .whitespace_normal(),
            )
            .into_any_element();
        self.row_with("All servers", content, cx)
    }

    /// The Gateway row's note: whether it is up, and who is connected.
    fn status_note(&self, cx: &App) -> (String, bool) {
        let state = self.state.read(cx);
        match &state.gateway.status {
            Status::Off => (
                "Agents reach your servers through one local address while this is on.".into(),
                false,
            ),
            Status::Starting => ("Starting…".into(), false),
            Status::Running(addr) => {
                let agents = state.gateway.agent_count();
                let who = match agents {
                    0 => "no agent connected".to_owned(),
                    1 => "1 agent connected".to_owned(),
                    n => format!("{n} agents connected"),
                };
                (format!("Listening on {addr} · {who}"), false)
            }
            Status::Failed(e) => (e.clone(), true),
        }
    }

    /// The Gateway section.
    pub(super) fn gateway(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let on = self.state.read(cx).gateway.settings.on;
        let (note, failed) = self.status_note(cx);
        let switch = h_flex()
            .gap(px(16.))
            .child(
                self.choice("gateway-on", "On", on, true, cx, |this, _, cx| {
                    this.state.update(cx, |s, cx| s.gateway_set_on(true, cx))
                }),
            )
            .child(
                self.choice("gateway-off", "Off", !on, true, cx, |this, _, cx| {
                    this.state.update(cx, |s, cx| s.gateway_set_on(false, cx))
                }),
            )
            .into_any_element();
        let status = v_flex()
            .gap(px(4.))
            .child(switch)
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(if failed { t.err } else { t.muted })
                    .child(note),
            )
            .into_any_element();
        let servers = self.servers_table(window, cx);
        v_flex()
            .child(self.row_with("Gateway", status, cx))
            .child(self.port_row(cx))
            .child(self.token_row(cx))
            .child(self.root_row(cx))
            .child(servers)
            .into_any_element()
    }

    /// One row per server: included or not, its path, the address, and the
    /// config to paste into an agent.
    fn servers_table(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *tokens(cx);
        let (plan, port, names) = {
            let state = self.state.read(cx);
            let records: Vec<_> = state.servers.iter().map(|s| s.record.clone()).collect();
            let plan = state.gateway.settings.plan(&records);
            let names: Vec<String> = records.iter().map(|r| r.name.clone()).collect();
            (plan, state.gateway.settings.port, names)
        };
        if plan.is_empty() {
            return div()
                .pt(px(12.))
                .child(muted(cx, 12., "Add a server to answer for it here."))
                .into_any_element();
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        for ((id, included), name) in plan.into_iter().zip(names) {
            let input = self.namespace_input(&id, &included.namespace, window, cx);
            let input = Input::new(&input)
                .id(SharedString::from(format!("namespace-{id}")))
                .h(px(26.));
            let url = format!("http://127.0.0.1:{port}/{}", included.namespace);
            let copied = (included.namespace.clone(), url.clone());
            let (on_id, off_id) = (id.clone(), id.clone());
            let error = self
                .namespace_error
                .clone()
                .filter(|(key, _)| *key == id)
                .map(|(_, e)| div().text_size(px(11.)).text_color(t.err).child(e));
            rows.push(
                v_flex()
                    .py(px(8.))
                    .border_b_1()
                    .border_color(t.hair)
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .gap(px(12.))
                            .items_center()
                            .child(div().w(px(110.)).flex_none().truncate().child(name))
                            .child(self.choice(
                                SharedString::from(format!("on-{id}")),
                                "On",
                                included.on,
                                true,
                                cx,
                                move |this, _, cx| {
                                    this.state.update(cx, |s, cx| {
                                        let _ = s.gateway_set_server(&on_id, Some(true), None, cx);
                                    })
                                },
                            ))
                            .child(self.choice(
                                SharedString::from(format!("off-{id}")),
                                "Off",
                                !included.on,
                                true,
                                cx,
                                move |this, _, cx| {
                                    this.state.update(cx, |s, cx| {
                                        let _ =
                                            s.gateway_set_server(&off_id, Some(false), None, cx);
                                    })
                                },
                            ))
                            .child(div().w(px(130.)).flex_none().child(input)),
                    )
                    .child(
                        h_flex()
                            .gap(px(12.))
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(crate::theme::mono(cx))
                                    .text_size(px(11.))
                                    .text_color(if included.on { t.fg } else { t.muted })
                                    .child(url),
                            )
                            .child(self.choice(
                                SharedString::from(format!("copy-config-{id}")),
                                "Copy config",
                                false,
                                included.on,
                                cx,
                                move |_, _, cx| {
                                    let config =
                                        mcp_exchange::gateway_config(std::slice::from_ref(&copied));
                                    crate::clip::copy(
                                        config.text(),
                                        "agent config · token left out",
                                        cx,
                                    );
                                },
                            )),
                    )
                    .children(error)
                    .into_any_element(),
            );
        }
        v_flex()
            .pt(px(12.))
            .child(
                div()
                    .pb(px(4.))
                    .text_size(px(13.))
                    .text_color(t.muted)
                    .child("Servers"),
            )
            .children(rows)
            .into_any_element()
    }

    /// The input for server `id`'s path, made on first use.
    fn namespace_input(
        &mut self,
        id: &str,
        namespace: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = self.namespaces.get(id) {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(namespace.to_owned()));
        let key = id.to_owned();
        self.subs.push(cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let wanted = input.read(cx).value().trim().to_owned();
                    this.apply_namespace(&key, &wanted, cx);
                }
            },
        ));
        self.namespaces.insert(id.to_owned(), input.clone());
        input
    }

    fn apply_namespace(&mut self, id: &str, wanted: &str, cx: &mut Context<Self>) {
        let result = self
            .state
            .update(cx, |s, cx| s.gateway_set_server(id, None, Some(wanted), cx));
        self.namespace_error = result.err().map(|e| (id.to_owned(), e));
        cx.notify();
    }

    /// Apply the port typed in its input.
    pub(super) fn apply_port(&mut self, cx: &mut Context<Self>) {
        let typed = self.port.read(cx).value().trim().to_owned();
        let result = match typed.parse::<u16>() {
            Ok(port) => self.state.update(cx, |s, cx| s.gateway_set_port(port, cx)),
            Err(_) => Err(format!(
                "a port is a number from {} to 65535",
                gateway::MIN_PORT
            )),
        };
        self.port_error = result.err();
        cx.notify();
    }
}
