//! The component kit: buttons, labels, status dots, meters and rows, all on
//! the tokens in `theme.rs`. Views compose these instead of styling ad hoc.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, Div, ElementId, FontWeight, IntoElement, ParentElement, SharedString, Stateful,
    Styled, div, prelude::*, pulsating_between, px, rgb,
};

use crate::icons::{Icon, icon, icon_sized};
use crate::theme;

// ---- Density ---------------------------------------------------------------------------

static COMPACT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Compact density: shorter rows and tighter spacing (Settings → Appearance).
pub fn set_compact(on: bool) {
    COMPACT.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn compact() -> bool {
    COMPACT.load(std::sync::atomic::Ordering::Relaxed)
}

/// Picks the comfortable or the compact value.
pub fn dense(comfortable: f32, compact_value: f32) -> f32 {
    if compact() { compact_value } else { comfortable }
}

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
        .h(px(dense(28., 25.)))
        .px(px(dense(11., 9.)))
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

/// A setting shown as quiet mono text that opens a menu of its choices
/// (the caller opens it): the chevron says there's more than one.
pub fn menu_chip(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(22.))
        .flex()
        .items_center()
        .gap(px(4.))
        .pl(px(7.))
        .pr(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(rgb(theme::line_strong()))
        .font_family(theme::MONO)
        .text_size(px(11.))
        .text_color(rgb(theme::text_2()))
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|s| s.bg(rgb(theme::hover())).text_color(rgb(theme::text())))
        .active(|s| s.bg(rgb(theme::selected())))
        .child(one_line(div()).child(text.into()))
        .child(icon_sized(Icon::ChevronDown, 10.).flex_none().text_color(rgb(theme::text_3())))
}

/// Cuts text to one line with an ellipsis, measured at the width it ends
/// up with. GPUI's `truncate()` keeps the first width it was measured at,
/// so text in a column or a shrinking row can show just "…" or get cut
/// without one; a one-line clamp is measured again when the width changes.
pub fn one_line<E: Styled>(el: E) -> E {
    el.min_w(px(0.)).overflow_hidden().line_clamp(1).text_ellipsis()
}

/// One action in a list of actions: icon, what it does, and a short note on
/// the right (what it would act on). Quieter than a button; the caller
/// attaches the click handler.
pub fn action_row(id: impl Into<ElementId>, glyph: Icon, text: impl Into<SharedString>, note: Option<String>) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(dense(30., 26.)))
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(8.))
        .rounded(px(6.))
        .text_size(px(13.))
        .text_color(rgb(theme::text()))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(theme::hover())))
        .active(|s| s.bg(rgb(theme::selected())))
        .child(icon_sized(glyph, 14.).flex_none().text_color(rgb(theme::text_3())))
        .child(div().flex_none().child(text.into()))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .justify_end()
                .children(note.map(|n| div().min_w(px(0.)).truncate().text_size(px(12.)).text_color(rgb(theme::text_3())).child(n))),
        )
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

/// Loading: three dots that light up in turn, in Monaged's dot language (like
/// the numerals). With motion off they sit at a steady half brightness; the
/// label next to them says what's happening.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: u32) -> AnyElement {
    let dot = (size / 4.4).clamp(2.5, 5.);
    let gap = dot * 0.75;
    let mut row = div().id(id).flex_none().h(px(size)).flex().items_center().gap(px(gap));
    for i in 0..3usize {
        let d = div().size(px(dot)).flex_none().rounded_full().bg(rgb(color));
        if !motion() {
            row = row.child(d.opacity(0.55));
            continue;
        }
        row = row.child(d.with_animation(("dot", i), Animation::new(Duration::from_millis(1100)).repeat(), move |el, t| {
            // Each dot peaks a third of a cycle after the one before it.
            let phase = (t - i as f32 / 3.0).rem_euclid(1.0);
            let bump = (1.0 - (phase * 2.0 - 1.0).abs()).powf(1.5);
            el.opacity(0.22 + 0.78 * bump)
        }));
    }
    row.into_any_element()
}

/// A thin progress bar: filled to `fraction`, or (None) a short segment that
/// slides along while it isn't known how far along the work is.
///
/// Built from flex-grow shares rather than percentage widths, and meant for a
/// flex column (`.flex().flex_col()`): inside a plain (block) `div()` without a
/// fixed width, the bar gets no width and stays empty.
pub fn progress_bar(id: impl Into<ElementId>, fraction: Option<f32>) -> AnyElement {
    let id: ElementId = id.into();
    let track = div().id(id.clone()).flex().w_full().h(px(3.)).rounded_full().overflow_hidden().bg(rgb(theme::line()));
    // A part of the bar taking `share` of its width.
    let part = |share: f32| {
        let mut d = div().h_full().flex_shrink_0();
        d.style().flex_grow = Some(share.max(0.0));
        d.style().flex_basis = Some(px(0.).into());
        d
    };
    match fraction {
        Some(f) => {
            let f = f.clamp(0., 1.);
            track.child(part(f).rounded_full().bg(rgb(theme::text_2()))).child(part(1.0 - f)).into_any_element()
        }
        None if !motion() => track.child(part(0.3).bg(rgb(theme::text_3()))).child(part(0.7)).into_any_element(),
        None => {
            // Space before the segment grows from 0 to 0.72 while the space after shrinks.
            let animation = || Animation::new(Duration::from_millis(1300)).repeat().with_easing(gpui::ease_in_out);
            track
                .child(part(0.0).with_animation(SharedString::from(format!("{id:?}-before")), animation(), |mut el, t| {
                    el.style().flex_grow = Some(0.72 * t);
                    el
                }))
                .child(part(0.28).rounded_full().bg(rgb(theme::text_2())))
                .child(part(0.72).with_animation(SharedString::from(format!("{id:?}-after")), animation(), |mut el, t| {
                    el.style().flex_grow = Some(0.72 * (1.0 - t));
                    el
                }))
                .into_any_element()
        }
    }
}

/// Where a step of a longer job is.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum StepState {
    Waiting,
    Running,
    Done,
    Failed,
    Skipped,
}

/// One step for `steps`.
#[derive(Clone, Debug)]
pub struct StepView {
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub state: StepState,
    /// While running: Some(None) shows a sliding bar, Some(Some(f)) a filled one.
    pub progress: Option<Option<f32>>,
}

/// A job as a list of steps, joined by a thin line: an outlined dot waits, a
/// pulsing dot runs (with a progress bar when there is one), a check is done,
/// red failed. Shows exactly where things are instead of a bare spinner.
pub fn steps(id: impl Into<SharedString>, items: &[StepView]) -> Div {
    let id: SharedString = id.into();
    let last = items.len().saturating_sub(1);
    div().flex().flex_col().children(items.iter().enumerate().map(|(i, step)| {
        let marker: AnyElement = match step.state {
            StepState::Waiting | StepState::Skipped => div().size(px(9.)).rounded_full().border_1().border_color(rgb(theme::line_strong())).into_any_element(),
            StepState::Running => {
                let dot = div().size(px(9.)).rounded_full().bg(rgb(theme::text()));
                if motion() {
                    dot.with_animation(
                        SharedString::from(format!("{id}-pulse-{i}")),
                        Animation::new(Duration::from_millis(1200)).repeat().with_easing(pulsating_between(0.3, 1.0)),
                        |el, t| el.opacity(t),
                    )
                    .into_any_element()
                } else {
                    dot.into_any_element()
                }
            }
            StepState::Done => icon_sized(Icon::Check, 14.).text_color(rgb(theme::text_2())).into_any_element(),
            StepState::Failed => icon_sized(Icon::Alert, 14.).text_color(rgb(theme::red_text())).into_any_element(),
        };
        let text_color = match step.state {
            StepState::Waiting | StepState::Skipped => theme::text_3(),
            StepState::Failed => theme::red_text(),
            _ => theme::text(),
        };
        div()
            .flex()
            .gap(px(12.))
            .child(
                div()
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(div().h(px(20.)).flex().items_center().justify_center().child(marker))
                    .when(i < last, |d| d.child(div().flex_1().min_h(px(10.)).w(px(1.)).bg(rgb(if step.state == StepState::Done { theme::text_3() } else { theme::line() })))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .pb(px(if i < last { 14. } else { 0. }))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .min_h(px(20.))
                            .flex()
                            .items_center()
                            .text_size(px(13.5))
                            .text_color(rgb(text_color))
                            .when(step.state == StepState::Running, |d| d.font_weight(FontWeight::MEDIUM))
                            .when(step.state == StepState::Skipped, |d| d.line_through())
                            .child(step.label.clone()),
                    )
                    .children(step.detail.clone().map(|d| div().text_size(px(12.)).text_color(rgb(theme::text_3())).child(d)))
                    .when_some(step.progress.filter(|_| step.state == StepState::Running), |d, p| {
                        d.child(div().pt(px(4.)).flex().flex_col().max_w(px(360.)).child(progress_bar(SharedString::from(format!("{id}-bar-{i}")), p)))
                    }),
            )
    }))
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
    div()
        .flex()
        .flex_col()
        .gap(px(dense(10., 8.)))
        .p(px(dense(14., 10.)))
        .rounded(px(10.))
        .border_1()
        .border_color(rgb(theme::line()))
        .bg(rgb(theme::raised()))
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
        .gap(px(dense(9., 7.)))
        .px(px(dense(16., 12.)))
        .py(px(dense(14., 10.)))
        .border_b_1()
        .border_color(rgb(theme::line()))
        .child(label(title))
}

/// A 28 px list row (sidebar, palette, lists).
pub fn row(id: impl Into<ElementId>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(dense(28., 24.)))
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

/// Opens a web page. Only http(s): Windows would run anything else it's
/// given (a path to an .exe, a share, a `search-ms:` query), and some links
/// come from servers and agents.
pub fn open_web(cx: &mut gpui::App, url: &str) {
    let url = url.trim();
    if is_web_link(url) {
        cx.open_url(url);
    } else {
        eprintln!("[ui] refused to open a link that isn't a web page: {url:?}");
    }
}

pub fn is_web_link(url: &str) -> bool {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")) && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod link_tests {
    #[test]
    fn only_web_links_count() {
        let ok = super::is_web_link;
        assert!(ok("https://github.com/x"));
        assert!(ok("http://localhost:3000"));
        assert!(!ok(r"C:\Windows\System32\calc.exe"));
        assert!(!ok(r"\\host\share\x.exe"));
        assert!(!ok("search-ms:query=x"));
        assert!(!ok("file:///C:/x.exe"));
        assert!(!ok("https://x.com/a b"));
    }
}
