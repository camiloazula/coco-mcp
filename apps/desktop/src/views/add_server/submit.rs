//! Turning the add-server form into a server spec and saving it.

use super::*;

impl AddServerForm {
    /// Build the name, the spec and a bearer token from the fields. Nothing
    /// is written: the model stores the token once the server is saved.
    pub(super) fn spec(
        &self,
        cx: &Context<Self>,
    ) -> Result<(String, ServerSpec, Option<String>), String> {
        let name = self.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return Err("Name is required".into());
        }
        if self.stdio {
            let (command, args) = server_fields::resolve(
                self.command_kept.as_ref(),
                &self.command.read(cx).value(),
                parse_command,
            )?;
            let cwd = server_fields::resolve(
                self.cwd_kept.as_ref(),
                &self.cwd.read(cx).value(),
                |text| Ok(parse_cwd(text)),
            )?;
            let env = server_fields::resolve(
                self.env_kept.as_ref(),
                &self.env.read(cx).value(),
                |text| parse_pairs(text, '='),
            )?;
            Ok((
                name,
                ServerSpec::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                },
                None,
            ))
        } else {
            let url = self.url.read(cx).value().trim().to_owned();
            if url.is_empty() {
                return Err("URL is required".into());
            }
            let headers = server_fields::resolve(
                self.headers_kept.as_ref(),
                &self.headers.read(cx).value(),
                |text| parse_pairs(text, ':'),
            )?;
            // Keep the entry the edited server has now, so stored OAuth
            // credentials survive; a new server gets a fresh one. Read from
            // the saved record, not from when the form opened: an earlier
            // save from this form may have made it.
            let keyring_id = self
                .edited(cx)
                .and_then(|ix| {
                    crate::state::keyring_id(&self.state.read(cx).servers[ix].record.spec)
                })
                .unwrap_or_else(mcp_auth::new_keyring_id);
            let (auth, token) = match self.auth {
                AuthKind::None => (AuthRef::None, None),
                AuthKind::Bearer => {
                    let token = self.token.read(cx).value().trim().to_owned();
                    if token.is_empty() && self.token_pending {
                        return Err(READING_TOKEN.into());
                    }
                    if token.is_empty() {
                        return Err("Token is required for bearer auth".into());
                    }
                    (AuthRef::Bearer { keyring_id }, Some(token))
                }
                AuthKind::OAuth => (AuthRef::OAuth { keyring_id }, None),
            };
            Ok((name, ServerSpec::Http { url, headers, auth }, token))
        }
    }

    /// Validate and save; the model connects once the server is saved. A
    /// save the model refuses keeps the form open with the reason.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        // One save at a time: a second press would add the server twice.
        if self.saving {
            return;
        }
        let (name, spec, token) = match self.spec(cx) {
            Ok(fields) => fields,
            Err(e) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
        };
        // What an earlier save was refused for no longer applies; the form
        // stays open through the connect, so it would be read as current.
        self.error = None;
        let editing = self.edited(cx);
        // The server was deleted while its settings were open: saving would
        // add it back, or land on another.
        if self.editing.is_some() && editing.is_none() {
            self.error = Some("This server was deleted.".into());
            cx.notify();
            return;
        }
        let protocol = self.protocol;
        let mut saving = self.state.update(cx, |state, cx| match editing {
            Some(ix) => state.update_server(ix, &name, spec, protocol, token, cx),
            None => state.add_server(&name, spec, protocol, token, cx),
        });
        // A refusal, and a save made in place, are already decided: show the
        // reason in this frame rather than after the next task runs.
        match saving.try_recv() {
            Ok(None) => {}
            Ok(Some(Err(e))) => {
                self.error = Some(e);
                cx.notify();
                return;
            }
            Ok(Some(Ok(()))) | Err(_) => return,
        }
        self.saving = true;
        cx.spawn(async move |this, cx| {
            let saved = saving.await;
            // On success the model has already left the form, so the form
            // may be gone by now.
            let _ = this.update(cx, |form, cx| {
                form.saving = false;
                if let Ok(Err(e)) = saved {
                    form.error = Some(e);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
