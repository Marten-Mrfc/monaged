//! Monaged's own icon set: 16 px line icons (1.4 px stroke, round caps),
//! drawn for this app and embedded in the binary. GPUI renders an SVG as a
//! mask, so an icon takes the colour of its `text_color`.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString, Styled, Svg, px, rgb, svg};

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[allow(dead_code)]
        pub enum Icon { $($variant),* }

        impl Icon {
            pub fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/", $file, ".svg")),* }
            }
        }

        fn embedded(path: &str) -> Option<&'static [u8]> {
            match path {
                $(concat!("icons/", $file, ".svg") => Some(include_bytes!(concat!("../assets/icons/", $file, ".svg")).as_slice()),)*
                _ => None,
            }
        }

        const ALL: &[&str] = &[$(concat!("icons/", $file, ".svg")),*];
    };
}

icons! {
    Folder => "folder",
    Branch => "branch",
    Merge => "merge",
    Layers => "layers",
    Agent => "agent",
    Flow => "flow",
    Diff => "diff",
    Board => "board",
    Sliders => "sliders",
    Search => "search",
    Plus => "plus",
    Play => "play",
    Stop => "stop",
    Check => "check",
    X => "x",
    ChevronRight => "chevron-right",
    ChevronDown => "chevron-down",
    External => "external",
    Eye => "eye",
    Send => "send",
    Ask => "ask",
    Terminal => "terminal",
    Edit => "edit",
    Shield => "shield",
    Push => "push",
    Gauge => "gauge",
    Sidebar => "sidebar",
    PanelRight => "panel-right",
    Minimize => "minimize",
    Maximize => "maximize",
    Restore => "restore",
    Refresh => "refresh",
    Archive => "archive",
    Alert => "alert",
    Clock => "clock",
    Key => "key",
    User => "user",
    Pin => "pin",
    Question => "question",
    Commit => "commit",
    Spinner => "spinner",
    More => "more",
    Copy => "copy",
    Info => "info",
    Keyboard => "keyboard",
    ArrowDown => "arrow-down",
    Trash => "trash",
    Flag => "flag",
    Dot => "dot",
    Paperclip => "paperclip",
    Image => "image",
    Lines => "lines",
    Hash => "hash",
    ArrowUp => "arrow-up",
}

/// Serves the embedded icons to GPUI.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(embedded(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ALL.iter().filter(|p| p.starts_with(path)).map(|p| SharedString::from(*p)).collect())
    }
}

/// An icon at the standard 16 px. SVGs don't inherit the parent's text
/// colour in GPUI, so it starts in the secondary grey; recolour with `.text_color(...)`.
pub fn icon(icon: Icon) -> Svg {
    svg().path(icon.path()).size(px(16.)).flex_none().text_color(rgb(crate::theme::text_2()))
}

/// A smaller icon (12–14 px) for dense rows and chevrons.
pub fn icon_sized(icon: Icon, size: f32) -> Svg {
    svg().path(icon.path()).size(px(size)).flex_none().text_color(rgb(crate::theme::text_2()))
}
