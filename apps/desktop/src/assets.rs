//! Asset source: gpui-kit's default component icons, a few extra Lucide
//! icons (ISC), and the app's own brand marks.
//!
//! GPUI renders an SVG as an alpha mask tinted with the element's text
//! colour, so the mark's outline and its eyes are separate files: one is
//! drawn in `fg`, the other in `accent`, and both follow the theme.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(pub ExtraIcons, [Trash, Pencil, Copy, Download]);

/// The app's own SVGs, embedded by path.
const BRAND: &[(&str, &[u8])] = &[
    (
        MARK,
        include_bytes!("../assets/icons/coco-mark.svg").as_slice(),
    ),
    (
        MARK_BOLD,
        include_bytes!("../assets/icons/coco-mark-bold.svg").as_slice(),
    ),
    (
        EYES,
        include_bytes!("../assets/icons/coco-eyes.svg").as_slice(),
    ),
    (
        EYES_BOLD,
        include_bytes!("../assets/icons/coco-eyes-bold.svg").as_slice(),
    ),
];

/// Outline of the mark, drawn in `fg`.
pub const MARK: &str = "icons/coco-mark.svg";
/// Heavier outline, for the mark inside the wordmark.
pub const MARK_BOLD: &str = "icons/coco-mark-bold.svg";
/// The two eyes, drawn in `accent`.
pub const EYES: &str = "icons/coco-eyes.svg";
/// Heavier eyes, matching [`MARK_BOLD`].
pub const EYES_BOLD: &str = "icons/coco-eyes-bold.svg";
/// Aspect ratio of the mark's viewBox (32 × 31).
pub const MARK_RATIO: f32 = 31. / 32.;

/// The app's asset source: brand marks, then extra icons, then gpui-kit's.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = BRAND.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.extend(
            BRAND
                .iter()
                .map(|(p, _)| *p)
                .filter(|p| p.starts_with(path))
                .map(SharedString::from),
        );
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_extra_and_default_icons_resolve() {
        for path in [MARK, MARK_BOLD, EYES, EYES_BOLD] {
            let svg = AppAssets.load(path).unwrap().expect(path);
            assert!(svg.starts_with(b"<svg"), "{path} is an SVG");
        }
        assert!(AppAssets.load("icons/trash.svg").unwrap().is_some());
        assert!(AppAssets.load("icons/pencil.svg").unwrap().is_some());
        assert!(AppAssets.load("icons/copy.svg").unwrap().is_some());
        assert!(AppAssets.load("icons/download.svg").unwrap().is_some());
        assert!(AppAssets.load("icons/search.svg").unwrap().is_some());
        // Unknown paths are either an error or `None`, never bytes.
        assert!(!matches!(AppAssets.load("icons/nope.svg"), Ok(Some(_))));
        assert!(AppAssets.list("icons/").unwrap().iter().any(|p| p == MARK));
    }
}
