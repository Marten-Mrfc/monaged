//! Design tokens and themes.
//!
//! Monochrome first, after Nothing OS and Resend: neutrals carry the
//! structure and hierarchy comes from contrast, size and weight. One accent,
//! red, only ever means "needs you" (an approval, a question, a failure, the
//! budget). Diffs are the only other colour. Every theme defines the same
//! roles, so views ask for a role (`theme::text_2()`), never a hex value.

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub key: &'static str,
    pub name: &'static str,
    pub blurb: &'static str,
    pub dark: bool,
    /// The main canvas (where you read).
    pub bg: u32,
    /// Side panels, title bar and status bar.
    pub panel: u32,
    /// Cards, inputs and popovers.
    pub raised: u32,
    pub hover: u32,
    pub selected: u32,
    /// Hairlines between regions and around cards.
    pub line: u32,
    /// Control outlines and dividers that must read.
    pub line_strong: u32,
    pub text: u32,
    /// Long-form reading text, a touch softer than `text`.
    pub text_body: u32,
    pub text_2: u32,
    /// Metadata; still 4.5:1 against every surface.
    pub text_3: u32,
    /// Unlit dots and disabled marks, never text.
    pub text_4: u32,
    /// The accent: dots and fills.
    pub red: u32,
    /// The accent as text.
    pub red_text: u32,
    /// A faint wash behind red things.
    pub red_soft: u32,
    /// Outline of red things (danger buttons, error callouts).
    pub red_line: u32,
    pub add: u32,
    pub add_bg: u32,
    pub del: u32,
    pub del_bg: u32,
    /// Background of inline code.
    pub code_bg: u32,
    /// Window shadow / scrim strength behind popovers (alpha 0–255).
    pub scrim: u8,
}

/// Near-black, after Nothing OS and Resend. The default.
pub const CARBON: Palette = Palette {
    key: "carbon",
    name: "Carbon",
    blurb: "Near-black, monochrome, one red",
    dark: true,
    bg: 0x0a0a0a,
    panel: 0x0f0f0f,
    raised: 0x161616,
    hover: 0x1c1c1c,
    selected: 0x242424,
    line: 0x1e1e1e,
    line_strong: 0x2f2f2f,
    text: 0xededed,
    text_body: 0xd4d4d4,
    text_2: 0xb4b4b4,
    text_3: 0x8a8a8a,
    text_4: 0x4a4a4a,
    red: 0xd71921,
    red_text: 0xff5a5f,
    red_soft: 0x2a1211,
    red_line: 0x5a1f1b,
    add: 0x7ccf95,
    add_bg: 0x0f1d13,
    del: 0xff7a7a,
    del_bg: 0x221012,
    code_bg: 0x1a1a1a,
    scrim: 0x99,
};

/// Softer dark grey, closer to an editor (Zed, IntelliJ): content lighter than chrome.
pub const GRAPHITE: Palette = Palette {
    key: "graphite",
    name: "Graphite",
    blurb: "Dim grey, easy on long sessions",
    dark: true,
    bg: 0x1d1d1f,
    panel: 0x171719,
    raised: 0x252528,
    hover: 0x2a2a2d,
    selected: 0x333337,
    line: 0x2a2a2d,
    line_strong: 0x3a3a3e,
    text: 0xececee,
    text_body: 0xd6d6da,
    text_2: 0xb6b6bc,
    text_3: 0x929299,
    text_4: 0x55555b,
    red: 0xe5484d,
    red_text: 0xff7b80,
    red_soft: 0x341a1c,
    red_line: 0x5c2a2d,
    add: 0x83d39c,
    add_bg: 0x1a2a1f,
    del: 0xff8a8a,
    del_bg: 0x34191b,
    code_bg: 0x2a2a2d,
    scrim: 0x88,
};

/// Warm dark, after the Claude desktop app.
pub const CLAY: Palette = Palette {
    key: "clay",
    name: "Clay",
    blurb: "Warm dark, like the Claude app",
    dark: true,
    bg: 0x262624,
    panel: 0x1f1e1d,
    raised: 0x30302e,
    hover: 0x343331,
    selected: 0x3b3a37,
    line: 0x343331,
    line_strong: 0x46443f,
    text: 0xfaf9f5,
    text_body: 0xe8e6dc,
    text_2: 0xc2c0b6,
    text_3: 0x9c9a92,
    text_4: 0x5e5c57,
    red: 0xe2614a,
    red_text: 0xf0907c,
    red_soft: 0x3d2620,
    red_line: 0x6a3a2e,
    add: 0x9ccb7a,
    add_bg: 0x2b3424,
    del: 0xee8884,
    del_bg: 0x40272a,
    code_bg: 0x30302e,
    scrim: 0x88,
};

/// Light, after Nothing's white phones: paper white, black ink, one red.
pub const PAPER: Palette = Palette {
    key: "paper",
    name: "Paper",
    blurb: "Light, black ink, one red",
    dark: false,
    bg: 0xffffff,
    panel: 0xf6f6f5,
    raised: 0xffffff,
    hover: 0xefefee,
    selected: 0xe7e7e6,
    line: 0xe9e9e8,
    line_strong: 0xd4d4d2,
    text: 0x111111,
    text_body: 0x262626,
    text_2: 0x4a4a4a,
    text_3: 0x6b6b6b,
    text_4: 0xc8c8c6,
    red: 0xd71921,
    red_text: 0xc4141c,
    red_soft: 0xfdeaea,
    red_line: 0xf3b9bb,
    add: 0x1a7f37,
    add_bg: 0xe7f6eb,
    del: 0xc4252c,
    del_bg: 0xfdebec,
    code_bg: 0xf1f1f0,
    scrim: 0x40,
};

pub const THEMES: [&Palette; 4] = [&CARBON, &GRAPHITE, &CLAY, &PAPER];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Switches the palette (the caller redraws).
pub fn set(key: &str) {
    let index = THEMES.iter().position(|t| t.key == key).unwrap_or(0);
    CURRENT.store(index, Ordering::Relaxed);
}

pub fn current() -> &'static Palette {
    THEMES[CURRENT.load(Ordering::Relaxed).min(THEMES.len() - 1)]
}

macro_rules! roles {
    ($($name:ident),* $(,)?) => { $(#[inline] pub fn $name() -> u32 { current().$name })* };
}

roles!(
    bg, panel, raised, hover, selected, line, line_strong, text, text_body, text_2, text_3, text_4, red, red_text,
    red_soft, red_line, add, add_bg, del, del_bg, code_bg
);

/// Text drawn on a `text()`-coloured fill (the primary button).
pub fn on_primary() -> u32 {
    current().bg
}

/// Hover colour of the primary button.
pub fn primary_hover() -> u32 {
    if current().dark { 0xd0d0d0 } else { 0x333333 }
}

/// A black scrim behind popovers, as RGBA.
pub fn scrim() -> u32 {
    current().scrim as u32
}

/// Marks the old `button(…, PRIMARY_BG)` calls as the primary action (see `main::button`).
pub const PRIMARY_BG: u32 = 0x0100_0001;

// ---- Type ---------------------------------------------------------------------------------
pub const SANS: &str = "Segoe UI Variable Text";
pub const DISPLAY: &str = "Segoe UI Variable Display";
pub const MONO: &str = "Cascadia Mono";
