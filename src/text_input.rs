//! Text input, adapted from GPUI's `examples/input.rs` (Apache-2.0).
//! Supports IME, selection, clipboard and mouse; Enter is handled by the parent
//! through the `Submit` action bound in `main.rs`.
//!
//! Two shapes:
//! - single-line (forms, the palette): one line that scrolls sideways;
//! - multi-line (`.multiline()`, the message boxes): wraps, grows up to
//!   `max_lines` and then scrolls, Shift+Enter adds a line, Up/Down move
//!   between lines and, at the top or bottom, through earlier messages.
//!   Pasting an image emits `InputEvent::PastedImage` for the parent.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, ContentMask, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine, SharedString, Style, TextAlign, TextRun,
    UTF16Selection, UnderlineStyle, Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, rgb, rgba, size,
};
use unicode_segmentation::*;

use crate::theme;

actions!(
    text_input,
    [
        Backspace, Delete, Left, Right, SelectLeft, SelectRight, SelectAll, Home, End, Paste, Cut, Copy, Up, Down, Newline,
        AcceptSuggestion, DeleteWordLeft
    ]
);

/// What a multi-line input tells its parent.
pub enum InputEvent {
    PastedImage(gpui::Image),
    /// Up/Down/Tab while the parent shows suggestions (slash commands).
    SuggestUp,
    SuggestDown,
    SuggestAccept,
    Changed,
}

impl EventEmitter<InputEvent> for TextInput {}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// How far the text is scrolled left so the cursor stays visible.
    scroll_x: Pixels,
    is_selecting: bool,
    /// Drawn without its own box (inside the composer or the palette).
    bare: bool,
    /// The form says this field is wrong: red outline until you edit it.
    invalid: bool,
    // Multi-line.
    multiline: bool,
    max_lines: usize,
    /// Hard lines as last laid out, with the byte offset each starts at.
    last_lines: Vec<(usize, WrappedLine)>,
    last_line_height: Pixels,
    scroll_y: Pixels,
    /// Earlier messages (oldest first) for Up/Down, and where we are in them.
    history: Vec<String>,
    history_pos: Option<usize>,
    draft: String,
    /// The parent shows suggestions: Up/Down/Tab go to it instead.
    pub suggesting: bool,
}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            scroll_x: px(0.),
            is_selecting: false,
            bare: false,
            invalid: false,
            multiline: false,
            max_lines: 10,
            last_lines: Vec::new(),
            last_line_height: px(22.),
            scroll_y: px(0.),
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
            suggesting: false,
        }
    }

    /// No border or background: the surrounding container draws the frame.
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    /// Wraps and grows; Shift+Enter adds a line.
    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    pub fn set_invalid(&mut self, invalid: bool, cx: &mut Context<Self>) {
        if self.invalid != invalid {
            self.invalid = invalid;
            cx.notify();
        }
    }

    pub fn is_invalid(&self) -> bool {
        self.invalid
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.scroll_y = px(0.);
        self.history_pos = None;
        cx.notify();
    }

    /// Replaces the text and puts the cursor at the end.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text: String = text.into();
        self.selected_range = text.len()..text.len();
        self.content = text.into();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    /// Messages you sent before, oldest first (Up walks back through them).
    pub fn set_history(&mut self, history: Vec<String>) {
        self.history = history;
        self.history_pos = None;
    }

    pub fn push_history(&mut self, text: String) {
        if self.history.last() != Some(&text) {
            self.history.push(text);
        }
        self.history_pos = None;
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let at = if self.multiline { self.content[..self.cursor_offset()].rfind('\n').map_or(0, |i| i + 1) } else { 0 };
        self.move_to(at, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let at = if self.multiline { self.content[cursor..].find('\n').map_or(self.content.len(), |i| cursor + i) } else { self.content.len() };
        self.move_to(at, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    /// Ctrl+Backspace: deletes the word before the cursor.
    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let cursor = self.cursor_offset();
            let before = &self.content[..cursor];
            let trimmed = before.trim_end();
            let start = trimmed.rfind(|c: char| c.is_whitespace()).map_or(0, |i| i + 1);
            self.select_to(start, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.replace_text_in_range(None, "\n", window, cx);
        }
    }

    fn accept_suggestion(&mut self, _: &AcceptSuggestion, _: &mut Window, cx: &mut Context<Self>) {
        if self.suggesting {
            cx.emit(InputEvent::SuggestAccept);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.suggesting {
            cx.emit(InputEvent::SuggestUp);
            return;
        }
        match self.vertical_target(-1.) {
            Some(index) => self.move_to(index, cx),
            None => self.history_back(cx),
        }
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.suggesting {
            cx.emit(InputEvent::SuggestDown);
            return;
        }
        match self.vertical_target(1.) {
            Some(index) => self.move_to(index, cx),
            None => self.history_forward(cx),
        }
    }

    fn history_back(&mut self, cx: &mut Context<Self>) {
        if self.history.is_empty() {
            self.move_to(0, cx);
            return;
        }
        let pos = match self.history_pos {
            None => {
                self.draft = self.content.to_string();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(p) => p - 1,
        };
        self.history_pos = Some(pos);
        let text = self.history[pos].clone();
        self.set_text(text, cx);
        self.history_pos = Some(pos);
    }

    fn history_forward(&mut self, cx: &mut Context<Self>) {
        let Some(pos) = self.history_pos else {
            self.move_to(self.content.len(), cx);
            return;
        };
        if pos + 1 < self.history.len() {
            let text = self.history[pos + 1].clone();
            self.set_text(text, cx);
            self.history_pos = Some(pos + 1);
        } else {
            let draft = std::mem::take(&mut self.draft);
            self.set_text(draft, cx);
            self.history_pos = None;
        }
    }

    /// The index one visual row up (-1) or down (+1) from the cursor, keeping
    /// its x; None when there's no row there (top or bottom).
    fn vertical_target(&self, direction: f32) -> Option<usize> {
        if !self.multiline || self.last_lines.is_empty() {
            return None;
        }
        let line_height = self.last_line_height;
        let cursor = self.content_point_for_index(self.cursor_offset())?;
        let target_y = cursor.y + line_height * direction + line_height / 2.;
        let total: Pixels = self.last_lines.iter().map(|(_, l)| l.size(line_height).height).fold(px(0.), |a, b| a + b);
        if target_y < px(0.) || target_y >= total {
            return None;
        }
        Some(self.index_for_content_point(point(cursor.x, target_y)))
    }

    /// Where the given byte index sits inside the laid-out text (top-left origin).
    fn content_point_for_index(&self, index: usize) -> Option<Point<Pixels>> {
        let line_height = self.last_line_height;
        let mut y = px(0.);
        for (start, line) in &self.last_lines {
            let end = start + line.len();
            if index >= *start && index <= end {
                let p = line.position_for_index(index - start, line_height)?;
                return Some(point(p.x, y + p.y));
            }
            y += line.size(line_height).height;
        }
        None
    }

    fn index_for_content_point(&self, position: Point<Pixels>) -> usize {
        let line_height = self.last_line_height;
        let mut y = px(0.);
        for (start, line) in &self.last_lines {
            let height = line.size(line_height).height;
            if position.y < y + height {
                let local = point(position.x.max(px(0.)), (position.y - y).max(px(0.)));
                let index = match line.closest_index_for_position(local, line_height) {
                    Ok(i) | Err(i) => i,
                };
                return start + index.min(line.len());
            }
            y += height;
        }
        self.content.len()
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        if let Some(text) = item.text() {
            let text = if self.multiline {
                text.replace("\r\n", "\n").replace('\r', "\n")
            } else {
                text.replace("\r\n", " ").replace(['\n', '\r'], " ")
            };
            self.replace_text_in_range(None, &text, window, cx);
            return;
        }
        if self.multiline {
            for entry in item.entries() {
                if let ClipboardEntry::Image(image) = entry {
                    cx.emit(InputEvent::PastedImage(image.clone()));
                }
            }
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected_range.clone()].to_string()));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed { self.selected_range.start } else { self.selected_range.end }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(bounds) = self.last_bounds.as_ref() else { return 0 };
        if self.multiline {
            let local = point(position.x - bounds.left(), position.y - bounds.top() + self.scroll_y);
            return self.index_for_content_point(local);
        }
        let Some(line) = self.last_layout.as_ref() else { return 0 };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left() + self.scroll_x)
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).rev().find_map(|(idx, _)| (idx < offset).then_some(idx)).unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content.grapheme_indices(true).find_map(|(idx, _)| (idx > offset).then_some(idx)).unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _ignore_disabled_input: bool, _window: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected_range), reversed: self.selection_reversed })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, _: &mut Window, cx: &mut Context<Self>) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content = (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        self.invalid = false;
        self.history_pos = None;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        self.content = (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.marked_range = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        cx.notify();
    }

    fn bounds_for_range(&mut self, range_utf16: Range<usize>, bounds: Bounds<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if self.multiline {
            let p = self.content_point_for_index(range.start)?;
            let top = bounds.top() + p.y - self.scroll_y;
            return Some(Bounds::from_corners(point(bounds.left() + p.x, top), point(bounds.left() + p.x + px(2.), top + self.last_line_height)));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(bounds.left() + last_layout.x_for_index(range.start) - self.scroll_x, bounds.top()),
            point(bounds.left() + last_layout.x_for_index(range.end) - self.scroll_x, bounds.bottom()),
        ))
    }

    fn character_index_for_point(&mut self, point: gpui::Point<Pixels>, _window: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        if self.multiline {
            return Some(self.offset_to_utf16(self.index_for_mouse_position(point)));
        }
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;
        // The layout shows the placeholder while the input is empty.
        if last_layout.text != self.content {
            return None;
        }
        let utf8_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

enum PrepaintState {
    Line { line: Option<ShapedLine>, scroll_x: Pixels, cursor: Option<PaintQuad>, selection: Option<PaintQuad> },
    Lines { lines: Vec<(usize, WrappedLine)>, scroll_y: Pixels, line_height: Pixels, cursor: Option<PaintQuad>, selection: Vec<PaintQuad> },
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The text to show (content or placeholder) and its runs, with the IME's
/// marked range underlined.
fn display_runs(input: &TextInput, window: &Window) -> (SharedString, Vec<TextRun>) {
    let style = window.text_style();
    let (display_text, text_color) =
        if input.content.is_empty() { (input.placeholder.clone(), rgb(theme::text_3()).into()) } else { (input.content.clone(), style.color) };
    let run = TextRun { len: display_text.len(), font: style.font(), color: text_color, background_color: None, underline: None, strikethrough: None };
    let runs = match input.marked_range.as_ref().filter(|_| !input.content.is_empty()) {
        Some(marked_range) => vec![
            TextRun { len: marked_range.start, ..run.clone() },
            TextRun {
                len: marked_range.end - marked_range.start,
                underline: Some(UnderlineStyle { color: Some(run.color), thickness: px(1.0), wavy: false }),
                ..run.clone()
            },
            TextRun { len: display_text.len() - marked_range.end, ..run },
        ]
        .into_iter()
        .filter(|run| run.len > 0)
        .collect(),
        None => vec![run],
    };
    (display_text, runs)
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if !input.multiline {
            style.size.height = window.line_height().into();
            return (window.request_layout(style, [], cx), ());
        }
        // Multi-line: as tall as the wrapped text, up to `max_lines`.
        let (text, runs) = display_runs(input, window);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let max_lines = input.max_lines;
        let layout = window.request_measured_layout(style, move |known, available, window, _cx| {
            let width = known.width.or(match available.width {
                gpui::AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let rows = window
                .text_system()
                .shape_text(text.clone(), font_size, &runs, width, None)
                .map(|lines| lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum::<usize>())
                .unwrap_or(1)
                .clamp(1, max_lines);
            gpui::size(width.unwrap_or(px(100.)), line_height * rows as f32)
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let (display_text, runs) = display_runs(input, window);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());

        if input.multiline {
            let line_height = window.line_height();
            let shaped = window.text_system().shape_text(display_text.clone(), font_size, &runs, Some(bounds.size.width), None).unwrap_or_default();
            // Hard lines with their start offsets (the '\n' between them is one byte).
            let mut lines = Vec::new();
            let mut start = 0;
            for line in shaped {
                let len = line.len();
                lines.push((start, line));
                start += len + 1;
            }
            let showing_placeholder = input.content.is_empty();
            let locate = |index: usize| -> Point<Pixels> {
                let mut y = px(0.);
                for (start, line) in &lines {
                    if index >= *start && index <= start + line.len() {
                        let p = line.position_for_index(index - start, line_height).unwrap_or_default();
                        return point(p.x, y + p.y);
                    }
                    y += line.size(line_height).height;
                }
                point(px(0.), y)
            };
            let cursor_at = if showing_placeholder { point(px(0.), px(0.)) } else { locate(cursor) };
            // Keep the cursor's row in view.
            let mut scroll_y = input.scroll_y;
            if cursor_at.y + line_height - scroll_y > bounds.size.height {
                scroll_y = cursor_at.y + line_height - bounds.size.height;
            }
            if cursor_at.y < scroll_y {
                scroll_y = cursor_at.y;
            }
            scroll_y = scroll_y.max(px(0.));
            let origin = point(bounds.left(), bounds.top() - scroll_y);
            let selection_color = rgba(0x6ea8fe40);
            let mut selection = Vec::new();
            let mut cursor_quad = None;
            if selected_range.is_empty() || showing_placeholder {
                cursor_quad = Some(fill(Bounds::new(point(origin.x + cursor_at.x, origin.y + cursor_at.y), size(px(2.), line_height)), rgb(theme::text())));
            } else {
                let a = locate(selected_range.start);
                let b = locate(selected_range.end);
                let rows = ((b.y - a.y) / line_height).round() as i32;
                for row in 0..=rows {
                    let y = a.y + line_height * row as f32;
                    let x0 = if row == 0 { a.x } else { px(0.) };
                    let x1 = if row == rows { b.x } else { bounds.size.width };
                    selection.push(fill(
                        Bounds::from_corners(point(origin.x + x0, origin.y + y), point(origin.x + x1.max(x0 + px(4.)), origin.y + y + line_height)),
                        selection_color,
                    ));
                }
            }
            return PrepaintState::Lines { lines, scroll_y, line_height, cursor: cursor_quad, selection };
        }

        let line = window.text_system().shape_line(display_text, font_size, &runs, None);
        // Keep the cursor in view: scroll just enough, never past the text's end.
        let cursor_pos = line.x_for_index(cursor);
        let width = bounds.size.width - px(2.);
        let mut scroll_x = input.scroll_x;
        if cursor_pos - scroll_x > width {
            scroll_x = cursor_pos - width;
        }
        if cursor_pos < scroll_x {
            scroll_x = cursor_pos;
        }
        scroll_x = scroll_x.min((line.width - width).max(px(0.))).max(px(0.));
        let left = bounds.left() - scroll_x;
        let (selection, cursor) = if selected_range.is_empty() {
            (None, Some(fill(Bounds::new(point(left + cursor_pos, bounds.top()), size(px(2.), bounds.bottom() - bounds.top())), rgb(theme::text()))))
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(left + line.x_for_index(selected_range.start), bounds.top()),
                        point(left + line.x_for_index(selected_range.end), bounds.bottom()),
                    ),
                    rgba(0x6ea8fe40),
                )),
                None,
            )
        };
        PrepaintState::Line { line: Some(line), scroll_x, cursor, selection }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        match prepaint {
            PrepaintState::Line { line, scroll_x, cursor, selection } => {
                let line = line.take().unwrap();
                let scroll_x = *scroll_x;
                // Clip to the input so long text doesn't spill past its border.
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    if let Some(selection) = selection.take() {
                        window.paint_quad(selection)
                    }
                    line.paint(point(bounds.left() - scroll_x, bounds.top()), window.line_height(), window, cx).unwrap();
                    if focus_handle.is_focused(window)
                        && let Some(cursor) = cursor.take()
                    {
                        window.paint_quad(cursor);
                    }
                });
                self.input.update(cx, |input, _cx| {
                    input.last_layout = Some(line);
                    input.last_bounds = Some(bounds);
                    input.scroll_x = scroll_x;
                });
            }
            PrepaintState::Lines { lines, scroll_y, line_height, cursor, selection } => {
                let (scroll_y, line_height) = (*scroll_y, *line_height);
                let lines = std::mem::take(lines);
                window.with_content_mask(Some(ContentMask { bounds }), |window| {
                    for quad in selection.drain(..) {
                        window.paint_quad(quad);
                    }
                    let mut y = bounds.top() - scroll_y;
                    for (_, line) in &lines {
                        let _ = line.paint(point(bounds.left(), y), line_height, TextAlign::Left, None, window, cx);
                        y += line.size(line_height).height;
                    }
                    if focus_handle.is_focused(window)
                        && let Some(cursor) = cursor.take()
                    {
                        window.paint_quad(cursor);
                    }
                });
                self.input.update(cx, |input, _cx| {
                    // The placeholder's layout doesn't map to the (empty) content.
                    input.last_lines = if input.content.is_empty() { Vec::new() } else { lines };
                    input.last_line_height = line_height;
                    input.last_bounds = Some(bounds);
                    input.scroll_y = scroll_y;
                });
            }
        }
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        div()
            .flex()
            .key_context(if self.multiline { "TextInput multiline" } else { "TextInput" })
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::accept_suggestion))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .when(!self.bare, |d| {
                d.px(px(10.))
                    .py(px(5.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(rgb(match (self.invalid, focused) {
                        (true, _) => theme::red(),
                        (false, true) => theme::text_3(),
                        (false, false) => theme::line_strong(),
                    }))
                    .bg(rgb(theme::raised()))
            })
            .text_color(rgb(theme::text()))
            .line_height(px(22.))
            .text_size(px(if self.bare { 14.5 } else { 13.5 }))
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Key bindings for the input; `ctrl-` because this targets Windows.
pub fn key_bindings() -> Vec<gpui::KeyBinding> {
    use gpui::KeyBinding;
    let ctx = Some("TextInput");
    let multi = Some("multiline");
    vec![
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, ctx),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("ctrl-a", SelectAll, ctx),
        KeyBinding::new("ctrl-v", Paste, ctx),
        KeyBinding::new("ctrl-c", Copy, ctx),
        KeyBinding::new("ctrl-x", Cut, ctx),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        // Only in message boxes, so the palette keeps its own Up/Down.
        KeyBinding::new("up", Up, multi),
        KeyBinding::new("down", Down, multi),
        KeyBinding::new("shift-enter", Newline, multi),
        KeyBinding::new("tab", AcceptSuggestion, multi),
    ]
}
