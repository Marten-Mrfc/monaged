//! The component kit: buttons, labels, status dots, meters and rows, all on
//! the tokens in `theme.rs`. Views compose these instead of styling ad hoc.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Div, ElementId, FontWeight, IntoElement, ParentElement, SharedString, Stateful,
    Styled, div, prelude::*, pulsating_between, px, rgb,
};

use crate::icons::{Icon, icon, icon_sized};
use crate::theme;

// ---- Text ----------------------------------------------------------------------------

/// Section label: small mono capitals, quiet (Nothing's raw labels).
pub fn label(text: impl Into<SharedString>) -> Div {
    let text: SharedString = text.into();
    div().font_family(theme::MONO).text_size(px(10.5)).text_color(rgb(theme::text_3())).child(SharedString::from(text.to_uppercase()))
}

/// Page title.
pub fn title(text: impl Into<SharedString>) -> Div {
    div().font_family(theme::DISPLAY).text_size(px(20.)).font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text())).child(text.into())
}

/// Header title (one level down).
pub fn heading(text: impl Into<SharedString>) -> Div {
    div().font_family(theme::DISPLAY).text_size(px(15.)).font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text())).child(text.into())
}

/// Secondary line under a title.
pub fn meta(text: impl Into<SharedString>) -> Div {
    div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(text.into())
}

/// Monospaced metadata (paths, branches, numbers).
pub fn mono(text: impl Into<SharedString>) -> Div {
    div().font_family(theme::MONO).text_size(px(11.5)).text_color(rgb(theme::text_3())).child(text.into())
}

/// A keyboard hint: `Ctrl K`.
pub fn kbd(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .font_family(theme::MONO)
        .text_size(px(11.))
        .text_color(rgb(theme::text_3()))
        .border_1()
        .border_color(rgb(theme::line_strong()))
        .rounded(px(4.))
        .px(px(5.))
        .line_height(px(17.))
        .child(text.into())
}

// ---- Buttons ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// The one main action in a view: white, like Resend and Nothing.
    Primary,
    /// Everything else that's a button: outlined.
    Secondary,
    /// Toolbar-style: no outline until hovered.
    Ghost,
    /// Destructive or "you must look": red text, red outline.
    Danger,
}

pub fn button(id: impl Into<ElementId>, kind: Kind, glyph: Option<Icon>, text: impl Into<SharedString>) -> Stateful<Div> {
    let (bg, fg, border) = colors(kind);
    let mut el = button_base(id, kind)
        .text_color(rgb(fg))
        .cursor_pointer();
    if let Some(bg) = bg {
        el = el.bg(rgb(bg));
    }
    if let Some(border) = border {
        el = el.border_1().border_color(rgb(border));
    }
    // Hover lifts, pressing sinks: feedback within the frame you click.
    el = match kind {
        Kind::Primary => el.hover(|s| s.bg(rgb(theme::primary_hover()))).active(|s| s.opacity(0.85)),
        Kind::Danger => el.hover(|s| s.bg(rgb(theme::red_soft()))).active(|s| s.opacity(0.8)),
        _ => el.hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text()))).active(|s| s.bg(rgb(theme::selected()))),
    };
    if let Some(glyph) = glyph {
        el = el.child(icon(glyph).size(px(14.)).text_color(rgb(fg)));
    }
    el.child(text.into())
}

fn colors(kind: Kind) -> (Option<u32>, u32, Option<u32>) {
    match kind {
        Kind::Primary => (Some(theme::text()), theme::on_primary(), Some(theme::text())),
        Kind::Secondary => (None, theme::text(), Some(theme::line_strong())),
        Kind::Ghost => (None, theme::text_2(), None),
        Kind::Danger => (None, theme::red_text(), Some(theme::red_line())),
    }
}

fn button_base(id: impl Into<ElementId>, kind: Kind) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(28.))
        .px(px(11.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .text_size(px(12.5))
        .when(kind == Kind::Primary, |d| d.font_weight(FontWeight::MEDIUM))
}

/// A button whose action is running: a spinner instead of the icon, no hover,
/// and no click handler (the caller attaches none). Same size as the idle button,
/// so nothing around it shifts.
pub fn busy_button(id: impl Into<ElementId>, kind: Kind, text: impl Into<SharedString>) -> Stateful<Div> {
    let (bg, fg, border) = colors(kind);
    let id: ElementId = id.into();
    let mut el = button_base(id.clone(), kind).text_color(rgb(fg)).cursor_default().opacity(0.8);
    if let Some(bg) = bg {
        el = el.bg(rgb(bg));
    }
    if let Some(border) = border {
        el = el.border_1().border_color(rgb(border));
    }
    el.child(spinner(id, 13., fg)).child(text.into())
}

/// A button that can't be used right now; say why in a tooltip.
pub fn disabled_button(id: impl Into<ElementId>, kind: Kind, glyph: Option<Icon>, text: impl Into<SharedString>) -> Stateful<Div> {
    let (_, _, border) = colors(if kind == Kind::Primary { Kind::Secondary } else { kind });
    let mut el = button_base(id, kind).text_color(rgb(theme::text_3())).cursor(gpui::CursorStyle::OperationNotAllowed);
    if border.is_some() {
        el = el.border_1().border_color(rgb(theme::line()));
    }
    if let Some(glyph) = glyph {
        el = el.child(icon(glyph).size(px(14.)).text_color(rgb(theme::text_4())));
    }
    el.child(text.into())
}

/// A 28 px square icon button for toolbars and rows.
pub fn icon_button(id: impl Into<ElementId>, glyph: Icon) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .group("icon-button")
        .cursor_pointer()
        .hover(|s| s.bg(rgb(theme::hover())))
        .active(|s| s.bg(rgb(theme::selected())))
        .child(icon(glyph).group_hover("icon-button", |s| s.text_color(rgb(theme::text()))))
}

// ---- Motion ----------------------------------------------------------------------------------

/// Windows' "Show animations" setting (Accessibility → Visual effects). When
/// it's off, nothing pulses or fades.
pub fn motion() -> bool {
    use std::sync::OnceLock;
    static MOTION: OnceLock<bool> = OnceLock::new();
    *MOTION.get_or_init(|| {
        #[cfg(windows)]
        {
            #[link(name = "user32")]
            unsafe extern "system" {
                fn SystemParametersInfoW(action: u32, param: u32, value: *mut core::ffi::c_void, win_ini: u32) -> i32;
            }
            const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
            let mut on: i32 = 1;
            let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&mut on as *mut i32).cast(), 0) };
            ok == 0 || on != 0
        }
        #[cfg(not(windows))]
        {
            true
        }
    })
}

/// Fades an element in once, quickly (popovers). Exits are instant.
pub fn fade_in(id: impl Into<ElementId>, el: impl IntoElement + gpui::Styled + 'static) -> AnyElement {
    if !motion() {
        return el.into_any_element();
    }
    el.with_animation(id, Animation::new(Duration::from_millis(140)).with_easing(gpui::ease_out_quint()), |el, t| el.opacity(t))
        .into_any_element()
}

/// The loading spinner: a three-quarter ring turning once every 0.9 s. With
/// motion off it stands still (the label next to it says what's happening).
pub fn spinner(id: impl Into<ElementId>, size: f32, color: u32) -> AnyElement {
    let ring = icon_sized(Icon::Spinner, size).text_color(rgb(color));
    if !motion() {
        return ring.into_any_element();
    }
    ring.with_animation(id, Animation::new(Duration::from_millis(900)).repeat(), |svg, t| {
        svg.with_transformation(gpui::Transformation::rotate(gpui::percentage(t)))
    })
    .into_any_element()
}

/// A placeholder bar where content is about to appear (skeleton loading):
/// the layout is already right, so nothing jumps when the real text arrives.
pub fn skeleton(id: impl Into<ElementId>, width: gpui::Length, height: f32) -> AnyElement {
    let bar = div().w(width).h(px(height)).flex_none().rounded(px(4.)).bg(rgb(theme::hover()));
    if !motion() {
        return bar.into_any_element();
    }
    bar.with_animation(id, Animation::new(Duration::from_millis(1400)).repeat().with_easing(pulsating_between(0.45, 1.0)), |el, t| {
        el.opacity(t)
    })
    .into_any_element()
}

/// A few skeleton lines shaped like a paragraph.
pub fn skeleton_text(id: &str, lines: usize) -> Div {
    let widths = [0.92, 0.78, 0.86, 0.55, 0.7];
    div().flex().flex_col().gap(px(9.)).children((0..lines).map(|i| {
        skeleton(SharedString::from(format!("{id}-{i}")), gpui::relative(widths[i % widths.len()]).into(), 9.)
    }))
}

// ---- Status --------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum State {
    /// Running a turn: white, gently pulsing.
    Working,
    /// Waiting for you: red.
    NeedsYou,
    /// Finished.
    Done,
    /// Not started, or no session.
    Idle,
}

/// The 7 px status dot. Working dots pulse (motion = "alive", nothing else moves).
pub fn status_dot(id: impl Into<ElementId>, state: State) -> AnyElement {
    let color = match state {
        State::Working => theme::text(),
        State::NeedsYou => theme::red(),
        State::Done => theme::text_3(),
        State::Idle => theme::text_4(),
    };
    let dot = div().flex_none().size(px(7.)).rounded_full().bg(rgb(color));
    if state == State::Working && motion() {
        dot.with_animation(
            id,
            Animation::new(Duration::from_millis(1600)).repeat().with_easing(pulsating_between(0.35, 1.0)),
            |el, opacity| el.opacity(opacity),
        )
        .into_any_element()
    } else {
        dot.into_any_element()
    }
}

/// Confidence as five dots; low scores light up red.
pub fn score_meter(score: u8) -> Div {
    let lit = if score <= 2 { theme::red() } else { theme::text() };
    div().flex().gap(px(3.)).children((1..=5u8).map(move |i| {
        div().size(px(5.)).rounded_full().bg(rgb(if i <= score { lit } else { theme::text_4() }))
    }))
}

/// Pipeline progress as eight small dots; the current stage pulses.
pub fn stage_dots(id: impl Into<SharedString>, done: usize, current: Option<usize>, total: usize) -> AnyElement {
    let id: SharedString = id.into();
    div()
        .flex()
        .gap(px(3.))
        .children((0..total).map(|i| {
            let dot = div().size(px(4.)).rounded_full();
            if Some(i) == current && motion() {
                dot.bg(rgb(theme::text()))
                    .with_animation(
                        SharedString::from(format!("{id}-{i}")),
                        Animation::new(Duration::from_millis(1600)).repeat().with_easing(pulsating_between(0.35, 1.0)),
                        |el, opacity| el.opacity(opacity),
                    )
                    .into_any_element()
            } else {
                dot.bg(rgb(if i < done { theme::text_2() } else { theme::text_4() })).into_any_element()
            }
        }))
        .into_any_element()
}

// ---- Containers ------------------------------------------------------------------------------

/// A raised card with a hairline; the only "box" in the system.
pub fn card() -> Div {
    div().flex().flex_col().gap(px(10.)).p(px(14.)).rounded(px(10.)).border_1().border_color(rgb(theme::line())).bg(rgb(theme::raised()))
}

/// An inline message inside a view: an error next to the thing that failed,
/// with room for a fix-it button. Red only when something needs you.
pub fn callout(tone: crate::agent::Tone, text: impl Into<SharedString>) -> Div {
    use crate::agent::Tone;
    let attention = !matches!(tone, Tone::Info);
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(if attention { theme::red_line() } else { theme::line() }))
        .bg(rgb(if attention { theme::red_soft() } else { theme::raised() }))
        .text_size(px(13.))
        .line_height(px(19.))
        .text_color(rgb(theme::text()))
        .child(
            div().pt(px(1.)).child(icon(if attention { Icon::Alert } else { Icon::Info }).text_color(rgb(if attention {
                theme::red_text()
            } else {
                theme::text_3()
            }))),
        )
        .child(div().flex_1().min_w(px(0.)).child(text.into()))
}

/// A section in a side panel: label on top, content below, hairline under.
pub fn panel_section(title: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(9.))
        .px(px(16.))
        .py(px(14.))
        .border_b_1()
        .border_color(rgb(theme::line()))
        .child(label(title))
}

/// A 28 px list row (sidebar, palette, lists).
pub fn row(id: impl Into<ElementId>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.))
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(10.))
        .rounded(px(6.))
        .text_size(px(13.))
        .cursor_pointer()
        .text_color(rgb(if selected { theme::text() } else { theme::text_2() }))
        .when(selected, |d| d.bg(rgb(theme::selected())))
        .when(!selected, |d| d.hover(|s| s.bg(rgb(theme::hover()))).active(|s| s.bg(rgb(theme::selected()))))
}

/// A chevron that reflects open/closed.
pub fn disclosure(open: bool) -> impl IntoElement {
    icon_sized(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 12.).text_color(rgb(theme::text_3()))
}

// ---- Form controls -------------------------------------------------------------------------

/// An on/off switch with its label; the caller attaches the click handler.
pub fn switch(id: impl Into<ElementId>, text: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_start()
        .gap(px(10.))
        .cursor_pointer()
        .text_size(px(13.))
        .text_color(rgb(theme::text_2()))
        .hover(|s| s.text_color(rgb(theme::text())))
        .child(
            div()
                .flex_none()
                .mt(px(1.))
                .w(px(30.))
                .h(px(17.))
                .rounded_full()
                .p(px(2.))
                .flex()
                .when(on, |d| d.justify_end().bg(rgb(theme::text())))
                .when(!on, |d| d.bg(rgb(theme::line_strong())))
                .child(div().size(px(13.)).rounded_full().bg(rgb(if on { theme::bg() } else { theme::text_3() }))),
        )
        .child(div().flex_1().min_w(px(0.)).line_height(px(19.)).child(text.into()))
}

/// One option of a small choice group (radio-like).
pub fn choice(id: impl Into<ElementId>, text: impl Into<SharedString>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(30.))
        .flex()
        .items_center()
        .px(px(12.))
        .rounded(px(6.))
        .border_1()
        .text_size(px(13.))
        .cursor_pointer()
        .border_color(rgb(if selected { theme::text_2() } else { theme::line_strong() }))
        .text_color(rgb(if selected { theme::text() } else { theme::text_2() }))
        .when(selected, |d| d.bg(rgb(theme::selected())))
        .when(!selected, |d| d.hover(|s| s.bg(rgb(theme::hover()))))
        .child(text.into())
}

/// A labelled text field with an optional hint underneath.
pub fn field(text: &'static str, hint: Option<&'static str>, input: gpui::Entity<crate::text_input::TextInput>) -> Div {
    field_checked(text, hint, input, None, None)
}

/// A field that can show what's wrong with it: the problem replaces the hint,
/// right under the box you need to fix (the input itself turns red).
pub fn field_checked(
    text: &'static str,
    hint: Option<&'static str>,
    input: gpui::Entity<crate::text_input::TextInput>,
    problem: Option<SharedString>,
    cx: Option<&gpui::App>,
) -> Div {
    // Editing the field clears its red outline; the message goes with it.
    let problem = problem.filter(|_| cx.is_none_or(|cx| input.read(cx).is_invalid()));
    let under: Option<AnyElement> = match problem {
        Some(problem) => Some(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(12.))
                .text_color(rgb(theme::red_text()))
                .child(icon_sized(Icon::Alert, 13.).text_color(rgb(theme::red_text())))
                .child(problem)
                .into_any_element(),
        ),
        None => hint.map(|h| div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(h).into_any_element()),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).text_color(rgb(theme::text_2())).child(text))
        .child(input)
        .children(under)
}
