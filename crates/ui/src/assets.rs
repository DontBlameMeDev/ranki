//! Embedded UI icons and the media asset source used by GPUI images.

use std::{borrow::Cow, fs, path::PathBuf};

use gpui::{AssetSource, Result, SharedString};

// Crane's source filter drops non-Rust assets, so the icon ships as a
// constant. `assets/rai.svg` stays the editable copy; RAI is its text-free
// build so the wordmark does not depend on installed fonts.
pub const RAI: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<svg
   width="800"
   height="800"
   viewBox="0 0 211.6667 211.66669"
   version="1.1"
   id="svg4"
   xmlns="http://www.w3.org/2000/svg"
   xmlns:svg="http://www.w3.org/2000/svg">
  <defs
     id="defs2">
    <filter
       id="glow"
       style="color-interpolation-filters:sRGB;"
       x="-0.082707436"
       y="-0.13798849"
       width="1.194759"
       height="1.3249346">
      <feOffset
         dx="3"
         dy="3"
         id="feOffset1" />
      <feGaussianBlur
         stdDeviation="3"
         result="blur"
         id="feGaussianBlur1" />
      <feFlood
         flood-color="rgb(0,0,0)"
         flood-opacity="1"
         result="flood"
         id="feFlood1" />
      <feComposite
         in="flood"
         in2="SourceGraphic"
         operator="in"
         result="composite"
         id="feComposite1" />
      <feBlend
         in="blur"
         in2="composite"
         mode="normal"
         result="fbSourceGraphic"
         id="feBlend1" />
      <feColorMatrix
         in="fbSourceGraphic"
         values="0 0 0 -1 0 0 0 0 -1 0 0 0 0 -1 0 0 0 0 1 0"
         id="feColorMatrix1" />
      <feColorMatrix
         in="fbSourceGraphic"
         values="1.3 0 0 0.0345 -0.15 0 1.3 0 0.0345 -0.15 0 0 1.3 0.0345 -0.15 0 0 0 1 0"
         id="feColorMatrix2" />
    </filter>
  </defs>
  <g
     id="text4"
     style="font-weight:bold;font-size:79.375px;font-family:Comfortaa;fill-rule:evenodd;stroke:#181825;stroke-width:2.51118;stroke-linejoin:round;paint-order:markers fill stroke;filter:url(#glow)"
     transform="matrix(1.0246445,0,0,0.96727946,41.324286,-7.9709619)"
     aria-label="rai">
    <path
       style="fill:#cba6f7"
       d="m 15.826117,147.98276 q -3.96875,0 -3.96875,-3.96875 v -35.48063 q 0,-3.96875 3.96875,-3.96875 3.96875,0 3.96875,3.96875 v 1.50813 q 2.38125,-2.77813 5.794375,-4.36563 3.4925,-1.5875 7.540625,-1.5875 4.7625,0 7.064375,1.5875 2.38125,1.50813 1.825625,3.65125 -0.396875,1.66688 -1.5875,2.2225 -1.190625,0.47625 -2.778125,0.15875 -5.08,-1.03187 -9.128125,-0.15875 -4.048125,0.87313 -6.429375,3.25438 -2.301875,2.38125 -2.301875,6.0325 v 23.1775 q 0,3.96875 -3.96875,3.96875 z"
       id="path4" />
    <path
       style="fill:#94e2d5"
       d="m 68.531123,148.30026 q -5.794375,0 -10.398125,-2.8575 -4.60375,-2.93688 -7.3025,-7.85813 -2.619375,-5.00062 -2.619375,-11.27125 0,-6.27062 2.8575,-11.27125 2.936875,-5.00062 7.858125,-7.85812 5.000625,-2.93688 11.191875,-2.93688 6.19125,0 11.1125,2.93688 4.92125,2.8575 7.77875,7.85812 2.936875,5.00063 2.936875,11.27125 v 17.70063 q 0,1.74625 -1.190625,2.93687 -1.11125,1.11125 -2.8575,1.11125 -1.74625,0 -2.936875,-1.11125 -1.11125,-1.19062 -1.11125,-2.93687 v -2.93688 q -2.778125,3.33375 -6.746875,5.31813 -3.889375,1.905 -8.5725,1.905 z m 1.5875,-7.14375 q 4.048125,0 7.223125,-1.905 3.175,-1.98438 5.000625,-5.31813 1.825625,-3.41312 1.825625,-7.62 0,-4.28625 -1.825625,-7.62 -1.825625,-3.41312 -5.000625,-5.31812 -3.175,-1.98438 -7.223125,-1.98438 -3.96875,0 -7.223125,1.98438 -3.175,1.905 -5.08,5.31812 -1.825625,3.33375 -1.825625,7.62 0,4.20688 1.825625,7.62 1.905,3.33375 5.08,5.31813 3.254375,1.905 7.223125,1.905 z"
       id="path5" />
    <path
       style="fill:#f38ba8"
       d="m 108.85356,147.98276 q -1.74625,0 -2.93687,-1.11125 -1.11125,-1.19063 -1.11125,-2.93688 v -35.32187 q 0,-1.82563 1.11125,-2.93688 1.19062,-1.11125 2.93687,-1.11125 1.82563,0 2.93688,1.11125 1.11125,1.11125 1.11125,2.93688 v 35.32187 q 0,1.74625 -1.11125,2.93688 -1.11125,1.11125 -2.93688,1.11125 z m 0,-50.482502 q -2.14312,0 -3.73062,-1.508125 -1.50813,-1.5875 -1.50813,-3.730625 0,-2.143125 1.50813,-3.65125 1.5875,-1.5875 3.73062,-1.5875 2.14313,0 3.65125,1.5875 1.5875,1.508125 1.5875,3.65125 0,2.143125 -1.5875,3.730625 -1.50812,1.508125 -3.65125,1.508125 z"
       id="path6" />
  </g>
</svg>
"#;

fn icon(body: &str) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{body}</svg>"
    )
}

const ICONS: &[(&str, &str)] = &[
    ("icon-plus", "<path d=\"M12 5v14M5 12h14\"/>"),
    (
        "icon-upload",
        "<path d=\"M12 16V4m-5 5 5-5 5 5\"/><path d=\"M4 16v3h16v-3\"/>",
    ),
    (
        "icon-search",
        "<circle cx=\"11\" cy=\"11\" r=\"7\"/><path d=\"m20 20-4-4\"/>",
    ),
    ("icon-chevron-down", "<path d=\"m6 9 6 6 6-6\"/>"),
    ("icon-chevron-right", "<path d=\"m9 6 6 6-6 6\"/>"),
    (
        "icon-grid",
        "<rect x=\"3\" y=\"3\" width=\"7\" height=\"7\" rx=\"1\"/><rect x=\"14\" y=\"3\" width=\"7\" height=\"7\" rx=\"1\"/><rect x=\"3\" y=\"14\" width=\"7\" height=\"7\" rx=\"1\"/><rect x=\"14\" y=\"14\" width=\"7\" height=\"7\" rx=\"1\"/>",
    ),
    (
        "icon-book",
        "<path d=\"M4 19.5A2.5 2.5 0 0 1 6.5 17H20\"/><path d=\"M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z\"/>",
    ),
    (
        "icon-chart",
        "<path d=\"M3 3v18h18\"/><path d=\"M7 15v3M12 9v9M17 5v13\"/>",
    ),
    (
        "icon-settings",
        "<circle cx=\"12\" cy=\"12\" r=\"3\"/><path d=\"M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6V21h-4v-.1a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.6-1H3v-4h.1a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9L4.2 7 7 4.2l.1.1a1.7 1.7 0 0 0 1.9.3 1.7 1.7 0 0 0 1-1.6V3h4v.1a1.7 1.7 0 0 0 1 1.6 1.7 1.7 0 0 0 1.9-.3l.1-.1L19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1H21v4h-.1a1.7 1.7 0 0 0-1.5 1z\"/>",
    ),
    ("icon-play", "<path d=\"m7 4 13 8-13 8V4z\"/>"),
    (
        "icon-volume",
        "<path d=\"M11 5 6 9H2v6h4l5 4V5z\"/><path d=\"M15.5 8.5a5 5 0 0 1 0 7\"/>",
    ),
    (
        "icon-more",
        "<circle cx=\"5\" cy=\"12\" r=\"1\"/><circle cx=\"12\" cy=\"12\" r=\"1\"/><circle cx=\"19\" cy=\"12\" r=\"1\"/>",
    ),
    ("icon-check", "<path d=\"m5 12 4 4L19 6\"/>"),
    (
        "icon-error",
        "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M9 9l6 6M15 9l-6 6\"/>",
    ),
    (
        "icon-warning",
        "<path d=\"M10.3 3.8 2.6 17a2 2 0 0 0 1.7 3h15.4a2 2 0 0 0 1.7-3L13.7 3.8a2 2 0 0 0-3.4 0z\"/><path d=\"M12 9v4\"/><path d=\"M12 17h.01\"/>",
    ),
    ("icon-x", "<path d=\"M18 6 6 18M6 6l12 12\"/>"),
    ("icon-arrow-left", "<path d=\"M19 12H5m6-7-7 7 7 7\"/>"),
    (
        "icon-trash",
        "<path d=\"M3 6h18M8 6V4h8v2m-9 0 1 14h8l1-14\"/>",
    ),
    (
        "icon-pencil",
        "<path d=\"M12 20h9\"/><path d=\"M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z\"/>",
    ),
    (
        "icon-undo",
        "<path d=\"M3 7v6h6\"/><path d=\"M3 13a9 9 0 1 0 3-7\"/>",
    ),
    ("icon-flag", "<path d=\"M5 21V4h13l-2 4 2 4H5\"/>"),
    (
        "icon-tag",
        "<path d=\"M20.6 13.4 12 22l-9-9V3h10l7.6 7.6a2 2 0 0 1 0 2.8z\"/><circle cx=\"7.5\" cy=\"7.5\" r=\"1\"/>",
    ),
    (
        "icon-bell",
        "<path d=\"M18 8a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9\"/><path d=\"M10 21h4\"/>",
    ),
    (
        "icon-clock",
        "<circle cx=\"12\" cy=\"12\" r=\"9\"/><path d=\"M12 7v5l3 2\"/>",
    ),
    (
        "icon-flame",
        "<path d=\"M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.072-2.143-.224-4.054 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.153.433-2.294 1-3a2.5 2.5 0 0 0 2.5 2.5z\"/>",
    ),
    (
        "icon-layers",
        "<path d=\"m12 2 9 5-9 5-9-5 9-5z\"/><path d=\"m3 12 9 5 9-5M3 17l9 5 9-5\"/>",
    ),
    ("icon-code", "<path d=\"m16 18 6-6-6-6M8 6l-6 6 6 6\"/>"),
    ("icon-math", "<path d=\"M18 7V4H6l6 8-6 8h12v-3\"/>"),
    (
        "icon-image",
        "<rect x=\"3\" y=\"3\" width=\"18\" height=\"18\" rx=\"2\"/><circle cx=\"9\" cy=\"9\" r=\"2\"/><path d=\"m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21\"/>",
    ),
    (
        "icon-text-color",
        "<path d=\"M4 20h16\"/><path d=\"M12 4 6 18\"/><path d=\"M12 4l6 14\"/><path d=\"M8.5 13h7\"/>",
    ),
    (
        "icon-highlight",
        "<path d=\"m9 11-6 6v3h9l3-3\"/><path d=\"m22 12-4.6 4.6a2 2 0 0 1-2.8 0l-5.2-5.2a2 2 0 0 1 0-2.8L14 4\"/>",
    ),
];

/// Serves embedded icons and local Anki media without touching the collection.
pub struct Assets {
    pub media_dir: PathBuf,
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "rai.svg" {
            return Ok(Some(Cow::Borrowed(RAI.as_bytes())));
        }
        if let Some((_, body)) = ICONS.iter().find(|(name, _)| *name == path) {
            let svg = icon(body);
            return Ok(Some(Cow::Owned(svg.into_bytes())));
        }
        let Some(file) = crate::card_view::media_path(&self.media_dir, path)
        else {
            return Ok(None);
        };
        Ok(fs::read(file).ok().map(Cow::Owned))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let Some(dir) = crate::card_view::media_path(&self.media_dir, path)
        else {
            return Ok(Vec::new());
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return Ok(Vec::new());
        };
        Ok(entries
            .filter_map(|entry| {
                Some(SharedString::from(
                    entry.ok()?.file_name().to_string_lossy().into_owned(),
                ))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource;

    use super::{Assets, RAI};

    #[test]
    fn embedded_icons_and_logo_are_available() {
        let assets = Assets {
            media_dir: std::path::PathBuf::from("."),
        };
        assert!(assets.load("rai.svg").unwrap().is_some());
        assert!(assets.load("icon-plus").unwrap().is_some());
        for name in [
            "icon-code",
            "icon-math",
            "icon-image",
            "icon-text-color",
            "icon-highlight",
        ] {
            assert!(
                assets.load(name).unwrap().is_some(),
                "{name} must be embedded"
            );
        }
        assert!(assets.load("missing-file").unwrap().is_none());
    }

    #[test]
    fn media_paths_cannot_escape_the_media_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let media = dir.path().join("media");
        std::fs::create_dir_all(&media).expect("media directory");
        let secret = dir.path().join("secret");
        std::fs::write(&secret, b"secret").expect("secret file");
        let assets = Assets { media_dir: media };

        assert!(assets.load("../secret").unwrap().is_none());
        assert!(assets.load(&secret.to_string_lossy()).unwrap().is_none());
        assert_eq!(
            assets.list("..").unwrap(),
            Vec::<gpui::SharedString>::new()
        );
        assert_eq!(
            assets.list(&secret.to_string_lossy()).unwrap(),
            Vec::<gpui::SharedString>::new()
        );
    }

    #[test]
    fn embedded_logo_uses_the_asset_colors_without_text() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/rai.svg");
        if let Ok(svg) = std::fs::read_to_string(path) {
            assert!(RAI.contains("viewBox=\"0 0 211.6667 211.66669\""));
            assert!(!RAI.contains("<text"));
            for color in ["#cba6f7", "#94e2d5", "#f38ba8"] {
                assert!(svg.contains(color));
                assert!(RAI.contains(color));
            }
        }
    }
}
