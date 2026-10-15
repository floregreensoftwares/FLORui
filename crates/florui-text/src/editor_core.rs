//! The editing core behind [`crate::TextEditor`]: a text buffer, its selection
//! and its Parley [`Layout`], kept up to date as the buffer, the width, the
//! alignment, the styles or the indent change.
//!
//! Parley's `PlainEditor` does the same job but builds its layout where this
//! crate cannot reach it, so the first line of an edited text could not be
//! indented (`text-indent`). This type builds the layout itself, with the
//! indent, and moves the caret and the selection with Parley's own
//! [`Selection`] and [`Cursor`], which is where the Unicode, bidirectional and
//! cluster logic lives. The operations follow `PlainEditor`'s driver, which is
//! dual-licensed Apache-2.0 / MIT like this crate.

use std::ops::Range;

use parley::editing::{Cursor, Selection};
use parley::{
    Affinity, Alignment, AlignmentOptions, BoundingBox, FontContext, IndentOptions, Layout,
    LayoutContext, StyleProperty, StyleSet,
};

type Brush = [u8; 4];

pub(crate) struct EditorCore {
    layout: Layout<Brush>,
    buffer: String,
    styles: StyleSet<Brush>,
    selection: Selection,
    /// Byte range of the IME preedit text in the buffer, while composing.
    compose: Option<Range<usize>>,
    /// An IME can ask for the caret to be hidden.
    show_cursor: bool,
    width: Option<f32>,
    font_size: f32,
    layout_dirty: bool,
    alignment: Alignment,
    indent: (f32, IndentOptions),
}

impl EditorCore {
    pub(crate) fn new(font_size: f32) -> Self {
        Self {
            layout: Layout::default(),
            buffer: String::new(),
            styles: StyleSet::new(font_size),
            selection: Selection::default(),
            compose: None,
            show_cursor: true,
            width: None,
            font_size,
            layout_dirty: true,
            alignment: Alignment::Start,
            indent: (0.0, IndentOptions::default()),
        }
    }

    pub(crate) fn driver<'a>(
        &'a mut self,
        font_cx: &'a mut FontContext,
        layout_cx: &'a mut LayoutContext<Brush>,
    ) -> Driver<'a> {
        Driver {
            editor: self,
            font_cx,
            layout_cx,
        }
    }

    pub(crate) fn raw_selection(&self) -> &Selection {
        &self.selection
    }

    pub(crate) fn is_composing(&self) -> bool {
        self.compose.is_some()
    }

    /// The text without the preedit region.
    pub(crate) fn text(&self) -> String {
        match &self.compose {
            Some(preedit) => format!(
                "{}{}",
                &self.buffer[..preedit.start],
                &self.buffer[preedit.end..]
            ),
            None => self.buffer.clone(),
        }
    }

    pub(crate) fn raw_text(&self) -> &str {
        &self.buffer
    }

    /// The selected text, or `None` for a caret or while composing.
    pub(crate) fn selected_text(&self) -> Option<&str> {
        if self.is_composing() || self.selection.is_collapsed() {
            return None;
        }
        self.buffer.get(self.selection.text_range())
    }

    pub(crate) fn set_text(&mut self, text: &str) {
        self.buffer.clear();
        self.buffer.push_str(text);
        self.layout_dirty = true;
        self.compose = None;
    }

    pub(crate) fn set_width(&mut self, width: Option<f32>) {
        if self.width != width {
            self.width = width;
            self.layout_dirty = true;
        }
    }

    pub(crate) fn set_alignment(&mut self, alignment: Alignment) {
        if self.alignment != alignment {
            self.alignment = alignment;
            self.layout_dirty = true;
        }
    }

    /// The `text-indent` of the text, in pixels, with its keywords.
    pub(crate) fn set_text_indent(&mut self, amount: f32, options: IndentOptions) {
        if self.indent != (amount, options) {
            self.indent = (amount, options);
            self.layout_dirty = true;
        }
    }

    pub(crate) fn edit_styles(&mut self) -> &mut StyleSet<Brush> {
        self.layout_dirty = true;
        &mut self.styles
    }

    /// The rectangles of the selection and the line of each.
    pub(crate) fn selection_geometry(&self) -> Vec<(BoundingBox, usize)> {
        self.selection.geometry(&self.layout)
    }

    /// The caret, unless an IME asked for it to be hidden.
    pub(crate) fn cursor_geometry(&self, size: f32) -> Option<BoundingBox> {
        self.show_cursor
            .then(|| self.selection.focus().geometry(&self.layout, size))
    }

    /// The area an IME should leave free: the preedit text, or the selection
    /// on the line of the caret, grown by a few average letters on each side.
    pub(crate) fn ime_cursor_area(&self) -> BoundingBox {
        let (area, focus) = if let Some(preedit) = &self.compose {
            let selection =
                Selection::new(self.cursor_at(preedit.start), self.cursor_at(preedit.end));
            let mut area = None;
            selection.geometry_with(&self.layout, |rect, _| {
                let area = area.get_or_insert(rect);
                *area = area.union(rect);
            });
            (
                area.unwrap_or_else(|| selection.focus().geometry(&self.layout, 0.)),
                selection.focus(),
            )
        } else {
            let focus = self.selection.focus().geometry(&self.layout, 0.);
            let mut area = focus;
            self.selection.geometry_with(&self.layout, |rect, _| {
                if rect.y0 == focus.y0 {
                    area = area.union(rect);
                }
            });
            (area, self.selection.focus())
        };
        let [upstream, downstream] = focus.logical_clusters(&self.layout);
        let font_size = downstream
            .or(upstream)
            .map(|cluster| cluster.run().font_size())
            .unwrap_or(self.font_size);
        // 0.6 of an em estimates the average advance.
        let inflate = 3. * 0.6 * font_size as f64;
        let editor_width = self.width.map(f64::from).unwrap_or(f64::INFINITY);
        BoundingBox {
            x0: (area.x0 - inflate).max(0.),
            x1: (area.x1 + inflate).min(editor_width),
            y0: area.y0,
            y1: area.y1,
        }
    }

    fn cursor_at(&self, index: usize) -> Cursor {
        if index >= self.buffer.len() {
            Cursor::from_byte_index(&self.layout, self.buffer.len(), Affinity::Upstream)
        } else {
            Cursor::from_byte_index(&self.layout, index, Affinity::Downstream)
        }
    }

    /// Keeps the preedit range on the same text after `old` was replaced by
    /// `new_len` bytes.
    fn update_compose_for_replaced_range(&mut self, old: Range<usize>, new_len: usize) {
        if new_len == old.len() {
            return;
        }
        let Some(compose) = &mut self.compose else {
            return;
        };
        if compose.end <= old.start {
            return;
        }
        if compose.start >= old.end {
            if new_len > old.len() {
                let diff = new_len - old.len();
                *compose = compose.start + diff..compose.end + diff;
            } else {
                let diff = old.len() - new_len;
                *compose = compose.start - diff..compose.end - diff;
            }
            return;
        }
        if new_len < old.len() {
            if compose.start >= old.start + new_len {
                self.compose = None;
                return;
            }
            compose.end = compose.end.min(old.start + new_len);
        }
    }

    fn set_selection(&mut self, selection: Selection) {
        self.selection = selection;
    }

    fn refresh_layout(&mut self, font_cx: &mut FontContext, layout_cx: &mut LayoutContext<Brush>) {
        if self.layout_dirty {
            self.update_layout(font_cx, layout_cx);
        }
    }

    fn update_layout(&mut self, font_cx: &mut FontContext, layout_cx: &mut LayoutContext<Brush>) {
        let mut builder = layout_cx.ranged_builder(font_cx, &self.buffer, 1.0, true);
        for property in self.styles.inner().values() {
            builder.push_default(property.to_owned());
        }
        if let Some(preedit) = &self.compose {
            builder.push(StyleProperty::Underline(true), preedit.clone());
        }
        self.layout = builder.build(&self.buffer);
        self.layout.set_text_indent(self.indent.0, self.indent.1);
        self.layout.break_all_lines(self.width);
        self.layout
            .align(self.alignment, AlignmentOptions::default());
        self.selection = self.selection.refresh(&self.layout);
        self.layout_dirty = false;
    }

    fn replace_selection(
        &mut self,
        font_cx: &mut FontContext,
        layout_cx: &mut LayoutContext<Brush>,
        text: &str,
    ) {
        let range = self.selection.text_range();
        let start = range.start;
        if self.selection.is_collapsed() {
            self.buffer.insert_str(start, text);
        } else {
            self.buffer.replace_range(range.clone(), text);
        }
        self.update_compose_for_replaced_range(range, text.len());
        self.update_layout(font_cx, layout_cx);
        let new_index = start.saturating_add(text.len());
        let affinity = if text.ends_with(['\n', '\r', '\u{2028}', '\u{2029}']) {
            Affinity::Downstream
        } else {
            Affinity::Upstream
        };
        self.set_selection(Cursor::from_byte_index(&self.layout, new_index, affinity).into());
    }
}

/// The operations that need an up-to-date layout, with the contexts that
/// build it.
pub(crate) struct Driver<'a> {
    editor: &'a mut EditorCore,
    font_cx: &'a mut FontContext,
    layout_cx: &'a mut LayoutContext<Brush>,
}

impl Driver<'_> {
    pub(crate) fn refresh_layout(&mut self) {
        self.editor.refresh_layout(self.font_cx, self.layout_cx);
    }

    pub(crate) fn layout(&mut self) -> &Layout<Brush> {
        self.refresh_layout();
        &self.editor.layout
    }

    fn update_layout(&mut self) {
        self.editor.update_layout(self.font_cx, self.layout_cx);
    }

    /// Runs a movement on the selection against the current layout.
    fn moving(&mut self, f: impl FnOnce(&Selection, &Layout<Brush>) -> Selection) {
        self.refresh_layout();
        let selection = f(&self.editor.selection, &self.editor.layout);
        self.editor.set_selection(selection);
    }

    // --- Editing ---
    pub(crate) fn insert_or_replace_selection(&mut self, text: &str) {
        self.editor
            .replace_selection(self.font_cx, self.layout_cx, text);
    }

    pub(crate) fn delete(&mut self) {
        if self.editor.selection.is_collapsed() {
            let range = self
                .editor
                .selection
                .focus()
                .logical_clusters(&self.editor.layout)[1]
                .as_ref()
                .map(|cluster| cluster.text_range())
                .and_then(|range| (!range.is_empty()).then_some(range));
            if let Some(range) = range {
                self.editor.buffer.replace_range(range.clone(), "");
                self.editor.update_compose_for_replaced_range(range, 0);
                self.update_layout();
            }
        } else {
            self.insert_or_replace_selection("");
        }
    }

    pub(crate) fn delete_word(&mut self) {
        if self.editor.selection.is_collapsed() {
            let focus = self.editor.selection.focus();
            let start = focus.index();
            let end = focus.next_logical_word(&self.editor.layout).index();
            self.delete_range_and_collapse(start, end);
        } else {
            self.insert_or_replace_selection("");
        }
    }

    pub(crate) fn backdelete(&mut self) {
        if !self.editor.selection.is_collapsed() {
            self.insert_or_replace_selection("");
            return;
        }
        let cluster = self
            .editor
            .selection
            .focus()
            .logical_clusters(&self.editor.layout)[0];
        let Some(cluster) = cluster else {
            return;
        };
        let range = cluster.text_range();
        let end = range.end;
        let start = if cluster.is_hard_line_break() || cluster.is_emoji() {
            // A newline sequence or an emoji goes as one.
            range.start
        } else {
            // Otherwise the previous character.
            let Some((start, _)) = self
                .editor
                .buffer
                .get(..end)
                .and_then(|text| text.char_indices().next_back())
            else {
                return;
            };
            start
        };
        self.delete_range_and_collapse(start, end);
    }

    pub(crate) fn backdelete_word(&mut self) {
        if self.editor.selection.is_collapsed() {
            let focus = self.editor.selection.focus();
            let end = focus.index();
            let start = focus.previous_logical_word(&self.editor.layout).index();
            self.delete_range_and_collapse(start, end);
        } else {
            self.insert_or_replace_selection("");
        }
    }

    fn delete_range_and_collapse(&mut self, start: usize, end: usize) {
        if self.editor.buffer.get(start..end).is_some() {
            self.editor.buffer.replace_range(start..end, "");
            self.editor.update_compose_for_replaced_range(start..end, 0);
            self.update_layout();
            self.editor.set_selection(
                Cursor::from_byte_index(&self.editor.layout, start, Affinity::Downstream).into(),
            );
        }
    }

    // --- IME ---
    pub(crate) fn set_compose(&mut self, text: &str, cursor: Option<(usize, usize)>) {
        let start = if let Some(preedit) = &self.editor.compose {
            self.editor.buffer.replace_range(preedit.clone(), text);
            preedit.start
        } else {
            let range = self.editor.selection.text_range();
            if self.editor.selection.is_collapsed() {
                self.editor.buffer.insert_str(range.start, text);
            } else {
                self.editor.buffer.replace_range(range.clone(), text);
            }
            range.start
        };
        self.editor.compose = Some(start..start + text.len());
        self.editor.show_cursor = cursor.is_some();
        self.update_layout();
        // Without a cursor the selection is a hidden caret at the preedit start.
        let cursor = cursor.unwrap_or((0, 0));
        let selection = Selection::new(
            self.editor.cursor_at(start + cursor.0),
            self.editor.cursor_at(start + cursor.1),
        );
        self.editor.set_selection(selection);
    }

    pub(crate) fn clear_compose(&mut self) {
        if let Some(preedit) = self.editor.compose.take() {
            self.editor.buffer.replace_range(preedit.clone(), "");
            self.editor.show_cursor = true;
            self.update_layout();
            let caret = self.editor.cursor_at(preedit.start).into();
            self.editor.set_selection(caret);
        }
    }

    // --- Movement and selection ---
    pub(crate) fn move_to_point(&mut self, x: f32, y: f32) {
        self.moving(|_, layout| Selection::from_point(layout, x, y));
    }

    pub(crate) fn move_to_text_start(&mut self) {
        self.moving(|s, l| s.move_lines(l, isize::MIN, false));
    }

    pub(crate) fn move_to_text_end(&mut self) {
        self.moving(|s, l| s.move_lines(l, isize::MAX, false));
    }

    pub(crate) fn move_to_line_start(&mut self) {
        self.moving(|s, l| s.line_start(l, false));
    }

    pub(crate) fn move_to_line_end(&mut self) {
        self.moving(|s, l| s.line_end(l, false));
    }

    pub(crate) fn move_up(&mut self) {
        self.moving(|s, l| s.previous_line(l, false));
    }

    pub(crate) fn move_down(&mut self) {
        self.moving(|s, l| s.next_line(l, false));
    }

    pub(crate) fn move_left(&mut self) {
        self.moving(|s, l| s.previous_visual(l, false));
    }

    pub(crate) fn move_right(&mut self) {
        self.moving(|s, l| s.next_visual(l, false));
    }

    pub(crate) fn move_word_left(&mut self) {
        self.moving(|s, l| s.previous_visual_word(l, false));
    }

    pub(crate) fn move_word_right(&mut self) {
        self.moving(|s, l| s.next_visual_word(l, false));
    }

    pub(crate) fn select_all(&mut self) {
        self.moving(|_, l| {
            Selection::from_byte_index(l, 0_usize, Affinity::default()).move_lines(
                l,
                isize::MAX,
                true,
            )
        });
    }

    pub(crate) fn select_to_text_start(&mut self) {
        self.moving(|s, l| s.move_lines(l, isize::MIN, true));
    }

    pub(crate) fn select_to_text_end(&mut self) {
        self.moving(|s, l| s.move_lines(l, isize::MAX, true));
    }

    pub(crate) fn select_to_line_start(&mut self) {
        self.moving(|s, l| s.line_start(l, true));
    }

    pub(crate) fn select_to_line_end(&mut self) {
        self.moving(|s, l| s.line_end(l, true));
    }

    pub(crate) fn select_up(&mut self) {
        self.moving(|s, l| s.previous_line(l, true));
    }

    pub(crate) fn select_down(&mut self) {
        self.moving(|s, l| s.next_line(l, true));
    }

    pub(crate) fn select_left(&mut self) {
        self.moving(|s, l| s.previous_visual(l, true));
    }

    pub(crate) fn select_right(&mut self) {
        self.moving(|s, l| s.next_visual(l, true));
    }

    pub(crate) fn select_word_left(&mut self) {
        self.moving(|s, l| s.previous_visual_word(l, true));
    }

    pub(crate) fn select_word_right(&mut self) {
        self.moving(|s, l| s.next_visual_word(l, true));
    }

    pub(crate) fn select_word_at_point(&mut self, x: f32, y: f32) {
        self.moving(|_, l| Selection::word_from_point(l, x, y));
    }

    pub(crate) fn select_hard_line_at_point(&mut self, x: f32, y: f32) {
        self.moving(|_, l| Selection::hard_line_from_point(l, x, y));
    }

    pub(crate) fn extend_selection_to_point(&mut self, x: f32, y: f32) {
        self.moving(|s, l| s.extend_to_point(l, x, y));
    }

    pub(crate) fn select_byte_range(&mut self, start: usize, end: usize) {
        if self.editor.buffer.is_char_boundary(start) && self.editor.buffer.is_char_boundary(end) {
            self.refresh_layout();
            let selection =
                Selection::new(self.editor.cursor_at(start), self.editor.cursor_at(end));
            self.editor.set_selection(selection);
        }
    }
}
