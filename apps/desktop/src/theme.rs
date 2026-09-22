//! Design tokens (the project's fixed palette), applied to gpui-kit's theme so
//! library widgets match, and exposed as a global for hand-drawn views.
//!
//! Every value here is part of the design; nothing is invented.

use std::borrow::Cow;

use gpui_kit::component::{ActiveTheme, Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Global, Hsla, Rgba, SharedString, Window, px, rgb, rgba};
use serde_json::json;

/// The design's colour tokens for one mode.
#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    /// Window and detail pane background.
    pub bg: Hsla,
    /// Title bar, sidebar, log drawer, status bar.
    pub sunk: Hsla,
    /// All 1px borders.
    pub hair: Hsla,
    /// Primary text.
    pub fg: Hsla,
    /// Secondary text.
    pub muted: Hsla,
    /// Selection bar, primary buttons, required marker, focus, connected dot.
    pub accent: Hsla,
    /// Text on accent.
    pub on_accent: Hsla,
    /// Inputs and raw JSON blocks.
    pub field: Hsla,
    /// Row hover.
    pub hover: Hsla,
    /// Selected row.
    pub sel: Hsla,
    /// JSON string values.
    pub str: Hsla,
    /// JSON numbers.
    pub num: Hsla,
    /// Error dot and error text.
    pub err: Hsla,
    /// Whether this is the dark set.
    pub dark: bool,
}

impl Tokens {
    /// Dark theme (first).
    pub fn dark() -> Self {
        Self {
            bg: rgb(0x161719).into(),
            sunk: rgb(0x111214).into(),
            hair: rgb(0x26282c).into(),
            fg: rgb(0xe3e4e6).into(),
            muted: rgb(0x8a8d93).into(),
            accent: rgb(0x5fbfbf).into(),
            on_accent: rgb(0x0c1414).into(),
            field: rgb(0x1c1d20).into(),
            hover: rgba(0xffffff08).into(),
            sel: rgba(0xffffff0d).into(),
            str: rgb(0xa9c9c9).into(),
            num: rgb(0xcfd2d6).into(),
            err: rgb(0xc66a5e).into(),
            dark: true,
        }
    }

    /// Light theme (second).
    pub fn light() -> Self {
        Self {
            bg: rgb(0xfafafa).into(),
            sunk: rgb(0xf2f2f3).into(),
            hair: rgb(0xe0e0e3).into(),
            fg: rgb(0x2a2b2e).into(),
            muted: rgb(0x75777d).into(),
            accent: rgb(0x1f8f8f).into(),
            on_accent: rgb(0xffffff).into(),
            field: rgb(0xffffff).into(),
            hover: rgba(0x00000008).into(),
            sel: rgba(0x0000000d).into(),
            str: rgb(0x2f7d7d).into(),
            num: rgb(0x3a3b3f).into(),
            err: rgb(0xb5533f).into(),
            dark: false,
        }
    }
}

impl Global for Tokens {}

/// The active token set.
pub fn tokens(cx: &App) -> &Tokens {
    cx.global::<Tokens>()
}

/// Monospace family in use (Geist Mono when installed, else the platform default).
pub fn mono(cx: &App) -> SharedString {
    cx.theme().mono_font_family.clone()
}

fn hex(color: Hsla) -> String {
    let Rgba { r, g, b, a } = color.to_rgb();
    let byte = |v: f32| (v * 255.0).round() as u8;
    if a >= 0.999 {
        format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b))
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            byte(r),
            byte(g),
            byte(b),
            byte(a)
        )
    }
}

/// gpui-kit theme JSON for one token set. Text selection colour is the
/// design's `rgba(90,200,200,.3)`.
fn theme_config(name: &str, t: &Tokens, font: &str, mono: &str) -> serde_json::Value {
    json!({
        "name": name,
        "mode": if t.dark { "dark" } else { "light" },
        "font.size": 13,
        "font.family": font,
        "mono_font.family": mono,
        "mono_font.size": 12,
        "radius": 3,
        "radius.lg": 3,
        "shadow": false,
        "colors": {
            "background": hex(t.bg),
            "foreground": hex(t.fg),
            "border": hex(t.hair),
            "input.border": hex(t.hair),
            "ring": hex(t.accent),
            "caret": hex(t.fg),
            "selection.background": "#5ac8c84d",
            "muted.background": hex(t.sunk),
            "muted.foreground": hex(t.muted),
            "accent.background": hex(t.sel),
            "accent.foreground": hex(t.fg),
            "primary.background": hex(t.accent),
            "primary.foreground": hex(t.on_accent),
            "primary.hover.background": hex(t.accent),
            "primary.active.background": hex(t.accent),
            "secondary.background": hex(t.field),
            "secondary.foreground": hex(t.fg),
            "secondary.hover.background": hex(t.field),
            "secondary.active.background": hex(t.field),
            "button.background": hex(t.field),
            "button.foreground": hex(t.fg),
            "button.hover.background": hex(t.field),
            "button.active.background": hex(t.field),
            "sidebar.background": hex(t.sunk),
            "sidebar.foreground": hex(t.fg),
            "sidebar.border": hex(t.hair),
            "sidebar.accent.background": hex(t.sel),
            "sidebar.accent.foreground": hex(t.fg),
            "sidebar.primary.background": hex(t.accent),
            "sidebar.primary.foreground": hex(t.on_accent),
            "title_bar.background": hex(t.sunk),
            "title_bar.border": hex(t.hair),
            "status_bar.background": hex(t.sunk),
            "status_bar.border": hex(t.hair),
            "list.background": hex(t.bg),
            "list.hover.background": hex(t.hover),
            "list.active.background": hex(t.sel),
            "list.active.border": hex(t.accent),
            "list.head.background": hex(t.sunk),
            "popover.background": hex(t.sunk),
            "popover.foreground": hex(t.fg),
            "tab_bar.background": hex(t.sunk),
            "tab.background": "#00000000",
            "tab.foreground": hex(t.muted),
            "tab.active.background": hex(t.bg),
            "tab.active.foreground": hex(t.fg),
            "danger.background": hex(t.err),
            "danger.foreground": hex(t.on_accent),
            "link": hex(t.accent),
            "scrollbar.background": "#00000000",
            "scrollbar.thumb.background": hex(t.hair),
            "scrollbar.thumb.hover.background": hex(t.muted),
            "switch.background": hex(t.hair),
            "skeleton.background": hex(t.field),
            "table.background": hex(t.bg),
            "table.head.background": hex(t.sunk),
            "table.active.background": hex(t.sel),
            "table.hover.background": hex(t.hover),
            "table.row.border": hex(t.hair),
            "window.border": hex(t.hair),
        },
        // The raw JSON editor's colours, the same the JSON trees use: keys
        // in the foreground, strings and numbers in their tokens, and the
        // punctuation, `null` and comments muted. Without this section the
        // editor falls back to gpui-component's own palette, which is
        // built for a white page.
        "highlight": {
            "editor.foreground": hex(t.fg),
            "editor.background": hex(t.field),
            "editor.active_line.background": hex(t.hover),
            "editor.line_number": hex(t.muted),
            "editor.active_line_number": hex(t.fg),
            "editor.invisible": hex(t.hair),
            "syntax": {
                "property": { "color": hex(t.fg) },
                "string": { "color": hex(t.str) },
                "string.escape": { "color": hex(t.str) },
                "number": { "color": hex(t.num) },
                "boolean": { "color": hex(t.num) },
                "constant": { "color": hex(t.muted) },
                "punctuation": { "color": hex(t.muted) },
                "comment": { "color": hex(t.muted) },
            }
        }
    })
}

/// Bundled faces, both under the SIL Open Font License 1.1 (texts next to
/// the files in `assets/fonts/`). Inter stands in for the neo-grotesque UI
/// face of the design; Geist Mono is the design's mono face. Embedding them
/// makes rendering identical on every platform and keeps proprietary system
/// fonts out of the app.
const BUNDLED_FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Regular.ttf"),
    include_bytes!("../assets/fonts/GeistMono-Medium.ttf"),
];
const UI_FONT: &str = "Inter";
const MONO_FONT: &str = "Geist Mono";
/// Free fallbacks, only reached if registering the bundled fonts fails.
const UI_FONTS: &[&str] = &[UI_FONT, "DejaVu Sans", "Liberation Sans"];
const MONO_FONTS: &[&str] = &[MONO_FONT, "DejaVu Sans Mono", "Liberation Mono"];

fn first_installed(candidates: &[&str], installed: &[String]) -> Option<String> {
    candidates
        .iter()
        .find(|c| installed.iter().any(|i| i == *c))
        .map(|c| (*c).to_owned())
}

/// Register both token sets with gpui-kit and activate the dark one.
pub fn install(cx: &mut App) -> anyhow::Result<()> {
    if let Err(e) = cx
        .text_system()
        .add_fonts(BUNDLED_FONTS.iter().map(|b| Cow::Borrowed(*b)).collect())
    {
        tracing::warn!("bundled fonts not registered: {e}");
    }
    let installed = cx.text_system().all_font_names();
    let font = first_installed(UI_FONTS, &installed).unwrap_or_else(|| UI_FONT.into());
    let mono = first_installed(MONO_FONTS, &installed).unwrap_or_else(|| MONO_FONT.into());
    let set = json!({
        "name": "Coco MCP",
        "author": "coco-mcp",
        "themes": [
            theme_config("Coco Dark", &Tokens::dark(), &font, &mono),
            theme_config("Coco Light", &Tokens::light(), &font, &mono),
        ]
    });
    ThemeRegistry::global_mut(cx).load_themes_from_str(&set.to_string())?;
    let (dark, light) = {
        let themes = ThemeRegistry::global(cx).themes();
        (
            themes.get("Coco Dark").cloned(),
            themes.get("Coco Light").cloned(),
        )
    };
    let (Some(dark), Some(light)) = (dark, light) else {
        anyhow::bail!("theme registration failed");
    };
    {
        let theme = Theme::global_mut(cx);
        theme.dark_theme = dark;
        theme.light_theme = light;
        // The design shows a tinted border on focus, not an outer ring.
        theme.focus_ring = false;
        theme.shadow = false;
    }
    cx.set_global(Tokens::dark());
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::global_mut(cx).focus_ring = false;
    Theme::global_mut(cx).radius = px(3.);
    Theme::global_mut(cx).radius_lg = px(3.);
    Theme::sync_base(cx);
    Ok(())
}

/// Switch between the two token sets.
pub fn set_dark(dark: bool, window: Option<&mut Window>, cx: &mut App) {
    cx.set_global(if dark {
        Tokens::dark()
    } else {
        Tokens::light()
    });
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        window,
        cx,
    );
    Theme::global_mut(cx).focus_ring = false;
    Theme::global_mut(cx).radius = px(3.);
    Theme::global_mut(cx).radius_lg = px(3.);
    Theme::sync_base(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_tokens() {
        assert_eq!(hex(Tokens::dark().bg), "#161719");
        assert_eq!(hex(Tokens::light().accent), "#1f8f8f");
        assert_eq!(hex(Tokens::dark().sel), "#ffffff0d");
    }

    #[test]
    fn both_themes_colour_the_editor_like_the_json_trees() {
        for t in [Tokens::dark(), Tokens::light()] {
            let config = theme_config("t", &t, "Inter", "Geist Mono");
            let syntax = &config["highlight"]["syntax"];
            assert_eq!(config["highlight"]["editor.foreground"], hex(t.fg));
            assert_eq!(syntax["property"]["color"], hex(t.fg));
            assert_eq!(syntax["string"]["color"], hex(t.str));
            assert_eq!(syntax["number"]["color"], hex(t.num));
            assert_eq!(syntax["punctuation"]["color"], hex(t.muted));
        }
    }

    #[test]
    fn font_selection_prefers_the_bundled_families() {
        let installed = vec![
            "Menlo".to_string(),
            "Helvetica Neue".to_string(),
            "Geist Mono".to_string(),
            "Inter".to_string(),
        ];
        assert_eq!(
            first_installed(MONO_FONTS, &installed).as_deref(),
            Some("Geist Mono")
        );
        assert_eq!(
            first_installed(UI_FONTS, &installed).as_deref(),
            Some("Inter")
        );
        // No proprietary family is ever a candidate.
        for name in [
            "Helvetica Neue",
            "Helvetica",
            "Segoe UI",
            "Menlo",
            "Consolas",
        ] {
            assert!(!UI_FONTS.contains(&name) && !MONO_FONTS.contains(&name));
        }
        assert_eq!(BUNDLED_FONTS.len(), 4);
        assert!(
            BUNDLED_FONTS.iter().all(|f| f.starts_with(b"\0\x01\0\0")),
            "TrueType"
        );
    }
}
