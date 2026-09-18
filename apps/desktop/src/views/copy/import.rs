//! Reading files in: server configs to import and snapshot files to compare
//! with.

use super::*;

impl Workspace {
    /// Ask for a client configuration file and add the servers it names.
    pub fn import_config_file(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            // Cancelled panel, closed channel or platform error: nothing to say.
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read = cx
                .background_spawn(async move { std::fs::read_to_string(&path) })
                .await;
            match read {
                Ok(text) => {
                    let _ = this.update(cx, |ws, cx| ws.import_config(&text, cx));
                }
                Err(e) => cx.update(|cx| {
                    crate::clip::announce_nothing(format!("Cannot read the file: {e}"), cx);
                }),
            }
        })
        .detach();
    }

    /// Read a client configuration and add the servers it names.
    /// Pick a snapshot file, one exported or committed as a baseline, and
    /// show how the selected server differs from it in the change banner.
    pub fn compare_snapshot_file(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Compare".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "a file".to_owned());
            let read = cx
                .background_spawn(async move {
                    let text = std::fs::read_to_string(&path)
                        .map_err(|e| format!("Cannot read the file: {e}"))?;
                    serde_json::from_str::<mcp_core::Snapshot>(&text)
                        .map_err(|e| format!("not a snapshot file: {e}"))
                })
                .await;
            match read {
                Ok(baseline) => {
                    let _ = this.update(cx, |ws, cx| {
                        ws.state
                            .update(cx, |s, cx| s.compare_with_snapshot(&baseline, name, cx));
                    });
                }
                Err(e) => cx.update(|cx| crate::clip::announce_nothing(e, cx)),
            }
        })
        .detach();
    }

    pub fn import_config(&mut self, text: &str, cx: &mut Context<Self>) {
        match mcp_exchange::read_client_config(text) {
            Ok(config) => {
                let imported = self.state.update(cx, |s, cx| s.import_servers(config, cx));
                cx.spawn(async move |_, cx| {
                    // Dropped only with the model, when there is no one to tell.
                    let Ok(result) = imported.await else {
                        return;
                    };
                    for note in &result.notes {
                        tracing::warn!("import: {note}");
                    }
                    // The status bar is where the person who imported looks:
                    // the summary, and the first problem when there was one.
                    let text = match result.notes.as_slice() {
                        [] => result.summary(),
                        [only] => format!("{} · {only}", result.summary()),
                        [first, rest @ ..] => {
                            format!("{} · {first} (and {} more)", result.summary(), rest.len())
                        }
                    };
                    cx.update(|cx| crate::clip::announce_nothing(text, cx));
                })
                .detach();
            }
            Err(e) => crate::clip::announce_nothing(e, cx),
        }
    }
}
