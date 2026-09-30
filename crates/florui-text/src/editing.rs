//! Real, Unicode/bidi-aware text editing — caret positioning,
//! selection, and the movement/deletion vocabulary a real `<input>` or
//! `<textarea>` needs.
//!
//! Wraps Parley's own `editing::PlainEditor` rather than hand-rolling
//! grapheme/word/bidi boundaries — it already implements exactly this
//! vocabulary, tested and bidi/cluster-correct. Without
//! [`TextEditor::set_width`] text never wraps, which is what a single-line
//! `<input>` wants; a `<textarea>` sets a width and gets wrapped lines,
//! with `Up`/`Down` and the point operations working across them.
//!
//! IME composition goes through [`TextEditOp::SetCompose`]/[`ClearCompose`]
//! — real Parley preedit state, spliced directly into the live buffer
//! (see [`Font::apply_text_edit`]'s own dispatch); [`TextEditor::is_composing`]
//! and [`Font::ime_cursor_area`] are the two things a caller needs to
//! know while it's active.

use parley::FontFamily as ParleyFontFamily;
use parley::editing::PlainEditor;
use parley::{FontWeight, StyleProperty};

use crate::{Font, FontFamily, ShapedGlyph, ShapedRun};

/// One buffer's worth of real, live-edited text — never wrapped, never
/// multi-style; see this module's own doc for why. Opaque: only
/// [`Font`]'s editing methods construct or inspect the [`PlainEditor`]
/// inside, the same way [`crate::CachedLayout`] hides its own `Layout`.
pub struct TextEditor(PlainEditor<[u8; 4]>);

/// A selection or caret position, in byte offsets into [`TextEditor`]'s
/// current text — `anchor == focus` is a collapsed selection (a plain
/// caret). Always in *logical* (source) byte order regardless of
/// bidirectional text's visual order; see `Selection::anchor`/`focus` in
/// Parley for the same distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteSelection {
    pub anchor: usize,
    pub focus: usize,
}

impl ByteSelection {
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.focus
    }

    /// The selected range in logical byte order — `anchor`/`focus` order
    /// depends on which end the user extended from (e.g. shift+left from
    /// the end selects backward), but a range consumer (deletion, copy)
    /// never cares which end is which, only the span.
    pub fn range(&self) -> std::ops::Range<usize> {
        self.anchor.min(self.focus)..self.anchor.max(self.focus)
    }
}

/// Every real editing operation this slice supports — one `TextEditOp`
/// maps to exactly one [`parley::editing::PlainEditorDriver`] call, so
/// this stays a thin dispatch table, not a second place editing logic
/// lives. `*Point`'s `x` is local to this editor's own top-left origin,
/// the same space [`Font::caret_rect`]'s own result is in.
pub enum TextEditOp {
    InsertOrReplace(String),
    Delete,
    Backdelete,
    DeleteWord,
    BackdeleteWord,
    MoveLeft,
    MoveRight,
    MoveWordLeft,
    MoveWordRight,
    MoveLineStart,
    MoveLineEnd,
    MoveTextStart,
    MoveTextEnd,
    SelectLeft,
    SelectRight,
    SelectWordLeft,
    SelectWordRight,
    SelectLineStart,
    SelectLineEnd,
    SelectTextStart,
    SelectTextEnd,
    SelectAll,
    MoveUp,
    MoveDown,
    SelectUp,
    SelectDown,
    MoveToPoint(f32, f32),
    SelectWordAtPoint(f32, f32),
    SelectHardLineAtPoint(f32, f32),
    ExtendSelectionToPoint(f32, f32),
    /// Sets the selection to an exact byte range — undo/redo's own way to
    /// restore a prior selection, not reachable from any real keyboard/
    /// mouse gesture (those all resolve relative to the current layout).
    SelectByteRange(usize, usize),
    /// Starts or updates IME preedit composition — `cursor` is byte
    /// offsets *relative to the start of `text`*, matching both Parley's
    /// own contract and winit's `Ime::Preedit`'s byte-indexed cursor
    /// directly, no conversion needed. Real callers never construct this
    /// with an empty `text` (Parley's own `set_compose` panics on one in
    /// debug builds) — see [`TextEditOp::ClearCompose`] instead.
    SetCompose(String, Option<(usize, usize)>),
    /// Ends composition: removes the preedit text, restores the caret.
    ClearCompose,
}

impl TextEditor {
    pub fn new(font_size: f32) -> Self {
        Self(PlainEditor::new(font_size))
    }

    /// Wraps text at `width` logical pixels, or never for `None`. A
    /// single-line `<input>` leaves this unset; a `<textarea>` sets it to
    /// its content width so long lines break like they do in a browser.
    pub fn set_width(&mut self, width: Option<f32>) {
        self.0.set_width(width);
    }

    /// Overwrites the whole buffer — used both to seed a freshly-created
    /// editor and to resync after an external rewrite (a rejected
    /// `Binding::request_update`, or the owner's value changing out from
    /// under this input). Does **not** itself move the caret: Parley's own
    /// `PlainEditor::set_text` leaves the existing byte-offset selection
    /// exactly as it was, clamped only if it now falls outside the new
    /// text's bounds — never snapped to the new end. A caller that wants
    /// "collapse to the end" (this slice's own resync policy; see
    /// `crates/florui-platform/src/text_input.rs`) must follow this with
    /// an explicit [`Font::apply_text_edit`] `MoveTextEnd`.
    pub fn set_text(&mut self, text: &str) {
        self.0.set_text(text);
    }

    pub fn text(&self) -> String {
        self.0.text().to_string()
    }

    pub fn selection(&self) -> ByteSelection {
        let selection = self.0.raw_selection();
        ByteSelection {
            anchor: selection.anchor().index(),
            focus: selection.focus().index(),
        }
    }

    /// The selected text, or `None` for a collapsed selection (a plain
    /// caret has nothing to copy/cut).
    pub fn selected_text(&self) -> Option<&str> {
        self.0.selected_text()
    }

    /// Whether a live IME preedit composition is in progress — the
    /// buffer already holds the composed-so-far text either way (see
    /// [`TextEditOp::SetCompose`]'s own doc), so this is what a caller
    /// checks before treating that text as final.
    pub fn is_composing(&self) -> bool {
        self.0.raw_compose().is_some()
    }
}

impl Font {
    /// Applies one real edit/movement/selection operation to `editor`,
    /// reshaping through this `Font`'s own already-owned font/layout
    /// context — the same fonts and fallback search every other
    /// `Font::shape*` call already uses, so an editable input's glyphs
    /// never come from a second, independently-configured source.
    /// Returns whether the buffer's *text* actually changed (a pure
    /// movement/selection op never does) — the caller's cue to commit a
    /// binding update and push an undo entry.
    pub fn apply_text_edit(
        &mut self,
        editor: &mut TextEditor,
        op: TextEditOp,
        family: FontFamily,
        font_weight: f32,
    ) -> bool {
        let family_name = self.family_name(family).to_owned();
        editor.0.edit_styles().insert(StyleProperty::FontFamily(
            ParleyFontFamily::named(&family_name).into_owned(),
        ));
        editor
            .0
            .edit_styles()
            .insert(StyleProperty::FontWeight(FontWeight::new(font_weight)));

        let text_before = editor.0.raw_text().to_owned();
        let mut driver = editor.0.driver(&mut self.font_cx, &mut self.layout_cx);
        match op {
            TextEditOp::InsertOrReplace(text) => driver.insert_or_replace_selection(&text),
            TextEditOp::Delete => driver.delete(),
            TextEditOp::Backdelete => driver.backdelete(),
            TextEditOp::DeleteWord => driver.delete_word(),
            TextEditOp::BackdeleteWord => driver.backdelete_word(),
            TextEditOp::MoveLeft => driver.move_left(),
            TextEditOp::MoveRight => driver.move_right(),
            TextEditOp::MoveWordLeft => driver.move_word_left(),
            TextEditOp::MoveWordRight => driver.move_word_right(),
            TextEditOp::MoveLineStart => driver.move_to_line_start(),
            TextEditOp::MoveLineEnd => driver.move_to_line_end(),
            TextEditOp::MoveTextStart => driver.move_to_text_start(),
            TextEditOp::MoveTextEnd => driver.move_to_text_end(),
            TextEditOp::SelectLeft => driver.select_left(),
            TextEditOp::SelectRight => driver.select_right(),
            TextEditOp::SelectWordLeft => driver.select_word_left(),
            TextEditOp::SelectWordRight => driver.select_word_right(),
            TextEditOp::SelectLineStart => driver.select_to_line_start(),
            TextEditOp::SelectLineEnd => driver.select_to_line_end(),
            TextEditOp::SelectTextStart => driver.select_to_text_start(),
            TextEditOp::SelectTextEnd => driver.select_to_text_end(),
            TextEditOp::SelectAll => driver.select_all(),
            TextEditOp::MoveUp => driver.move_up(),
            TextEditOp::MoveDown => driver.move_down(),
            TextEditOp::SelectUp => driver.select_up(),
            TextEditOp::SelectDown => driver.select_down(),
            TextEditOp::MoveToPoint(x, y) => driver.move_to_point(x, y),
            TextEditOp::SelectWordAtPoint(x, y) => driver.select_word_at_point(x, y),
            TextEditOp::SelectHardLineAtPoint(x, y) => driver.select_hard_line_at_point(x, y),
            TextEditOp::ExtendSelectionToPoint(x, y) => driver.extend_selection_to_point(x, y),
            TextEditOp::SelectByteRange(start, end) => driver.select_byte_range(start, end),
            TextEditOp::SetCompose(text, cursor) => driver.set_compose(&text, cursor),
            TextEditOp::ClearCompose => driver.clear_compose(),
        }
        editor.0.raw_text() != text_before
    }

    /// The laid-out text's total size in logical pixels — what a scroll
    /// range is measured against. For an editor with no wrap width the
    /// width is the longest line's.
    pub fn content_size(&mut self, editor: &mut TextEditor) -> (f32, f32) {
        let mut driver = editor.0.driver(&mut self.font_cx, &mut self.layout_cx);
        driver.refresh_layout();
        let layout = driver.layout();
        (layout.full_width(), layout.height())
    }

    /// The area of the current IME preedit composition, or the current
    /// selection if not composing — Parley's own `ime_cursor_area`, the
    /// only geometry it exposes for an arbitrary (non-selection) byte
    /// range. Safe to call unconditionally; gate *use* of the result on
    /// [`TextEditor::is_composing`] at the call site.
    pub fn ime_cursor_area(&mut self, editor: &mut TextEditor) -> (f32, f32, f32, f32) {
        editor
            .0
            .driver(&mut self.font_cx, &mut self.layout_cx)
            .refresh_layout();
        let rect = editor.0.ime_cursor_area();
        (
            rect.x0 as f32,
            rect.y0 as f32,
            rect.x1 as f32,
            rect.y1 as f32,
        )
    }

    /// The caret's own rect (`x0/y0/x1/y1`, zero-width — a caller draws
    /// its own visible width) at `editor`'s current, collapsed-or-not
    /// selection focus — `None` only if the editor has asked to hide it,
    /// which real IME composition on some platforms does (see
    /// [`TextEditor::is_composing`]).
    pub fn caret_rect(&mut self, editor: &mut TextEditor) -> Option<(f32, f32, f32, f32)> {
        editor
            .0
            .driver(&mut self.font_cx, &mut self.layout_cx)
            .refresh_layout();
        editor.0.cursor_geometry(1.0).map(|rect| {
            (
                rect.x0 as f32,
                rect.y0 as f32,
                rect.x1 as f32,
                rect.y1 as f32,
            )
        })
    }

    /// One rect per selected run on this (single) line — real single-line
    /// text never needs more than one, but the type stays a `Vec` rather
    /// than an `Option` to match Parley's own general (multi-line)
    /// contract without a lossy cast.
    pub fn selection_rects(&mut self, editor: &mut TextEditor) -> Vec<(f32, f32, f32, f32)> {
        editor
            .0
            .driver(&mut self.font_cx, &mut self.layout_cx)
            .refresh_layout();
        editor
            .0
            .selection_geometry()
            .into_iter()
            .map(|(rect, _line)| {
                (
                    rect.x0 as f32,
                    rect.y0 as f32,
                    rect.x1 as f32,
                    rect.y1 as f32,
                )
            })
            .collect()
    }

    /// The real glyphs to paint for `editor`'s current text — same
    /// [`ShapedRun`] shape [`Font::shape`]/[`Font::shape_wrapped`] already
    /// return, so a paint routine needs no separate code path for an
    /// editable input's text versus any other shaped run.
    pub fn shaped_runs_for_edit(&mut self, editor: &mut TextEditor) -> Vec<ShapedRun> {
        let mut driver = editor.0.driver(&mut self.font_cx, &mut self.layout_cx);
        driver.refresh_layout();
        let layout = driver.layout();
        let mut runs = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                let parley::PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let run = glyph_run.run();
                let glyphs = glyph_run
                    .positioned_glyphs()
                    .map(|glyph| ShapedGlyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                    })
                    .collect();
                runs.push(ShapedRun {
                    font: run.font().clone(),
                    font_size: run.font_size(),
                    normalized_coords: run.normalized_coords().to_vec(),
                    glyphs,
                });
            }
        }
        runs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontFamily;

    #[test]
    fn select_byte_range_restores_an_exact_prior_selection() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace("hello".to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        font.apply_text_edit(
            &mut editor,
            TextEditOp::SelectByteRange(1, 3),
            FontFamily::SansSerif,
            400.0,
        );
        assert_eq!(
            editor.selection(),
            ByteSelection {
                anchor: 1,
                focus: 3
            }
        );
        assert_eq!(editor.selected_text(), Some("el"));
    }

    #[test]
    fn insert_or_replace_appends_at_the_end_of_an_empty_editor() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace("hello".to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        assert_eq!(editor.text(), "hello");
        assert_eq!(
            editor.selection(),
            ByteSelection {
                anchor: 5,
                focus: 5
            }
        );
    }

    #[test]
    fn select_all_then_delete_clears_the_buffer() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace("hello".to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        font.apply_text_edit(
            &mut editor,
            TextEditOp::SelectAll,
            FontFamily::SansSerif,
            400.0,
        );
        assert_eq!(editor.selected_text(), Some("hello"));
        let changed = font.apply_text_edit(
            &mut editor,
            TextEditOp::Delete,
            FontFamily::SansSerif,
            400.0,
        );
        assert!(changed);
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn a_pure_movement_op_reports_no_text_change() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace("hi".to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        let changed = font.apply_text_edit(
            &mut editor,
            TextEditOp::MoveLeft,
            FontFamily::SansSerif,
            400.0,
        );
        assert!(!changed, "moving the caret must not itself change the text");
        assert_eq!(
            editor.selection(),
            ByteSelection {
                anchor: 1,
                focus: 1
            }
        );
    }

    #[test]
    fn caret_rect_is_a_real_nonzero_width_rect_once_drawn() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace("hi".to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        let rect = font.caret_rect(&mut editor).expect("cursor is not hidden");
        assert!(rect.3 > rect.1, "caret must have a real, nonzero height");
    }

    #[test]
    fn set_compose_starts_composing_but_stays_out_of_the_real_committed_text() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        assert!(!editor.is_composing());
        let changed = font.apply_text_edit(
            &mut editor,
            TextEditOp::SetCompose("n".to_string(), Some((1, 1))),
            FontFamily::SansSerif,
            400.0,
        );
        assert!(editor.is_composing());
        // The buffer really holds "n" (so it paints), but `text()` -- the
        // real, committed value a caller reports to a Binding/oninput --
        // deliberately excludes preedit content (Parley's own contract:
        // "the in-progress IME content is not itself what the user
        // intends to write").
        assert!(
            changed,
            "the raw buffer did change, even if text() hides it"
        );
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn clear_compose_really_removes_the_preedit_text_not_just_from_text() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::SetCompose("n".to_string(), Some((1, 1))),
            FontFamily::SansSerif,
            400.0,
        );
        font.apply_text_edit(
            &mut editor,
            TextEditOp::ClearCompose,
            FontFamily::SansSerif,
            400.0,
        );
        assert!(!editor.is_composing());
        // `text()` was already "" the whole time (it always excludes an
        // active preedit) -- the real proof `clear_compose` removed the
        // preedit from the *buffer*, not just Parley's own read-side
        // filter, is that new real text afterward isn't prefixed with it.
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace("hi".to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        assert_eq!(editor.text(), "hi");
    }

    #[test]
    fn ime_cursor_area_is_a_real_rect_while_composing() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::SetCompose("n".to_string(), Some((1, 1))),
            FontFamily::SansSerif,
            400.0,
        );
        let rect = font.ime_cursor_area(&mut editor);
        assert!(rect.3 > rect.1, "must have a real, nonzero height");
    }

    #[test]
    fn set_text_replaces_the_buffer_without_moving_the_caret_on_its_own() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace("original".to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        editor.set_text("rewritten");
        assert_eq!(editor.text(), "rewritten");
        assert_eq!(
            editor.selection(),
            ByteSelection {
                anchor: 8,
                focus: 8
            },
            "set_text alone leaves the prior byte offset in place if still valid \
             for the new text -- a caller wanting the end must ask for it explicitly"
        );
    }

    #[test]
    fn move_text_end_after_set_text_collapses_the_caret_to_the_new_end() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace("original".to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        editor.set_text("rewritten");
        font.apply_text_edit(
            &mut editor,
            TextEditOp::MoveTextEnd,
            FontFamily::SansSerif,
            400.0,
        );
        assert_eq!(
            editor.selection(),
            ByteSelection {
                anchor: 9,
                focus: 9
            }
        );
    }

    #[test]
    fn shaped_runs_for_edit_matches_plain_shape_for_the_same_text() {
        let mut font = Font::load_embedded();
        let mut editor = TextEditor::new(16.0);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace("Hi".to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        let edited_runs = font.shaped_runs_for_edit(&mut editor);
        let plain = font.shape(FontFamily::SansSerif, "Hi", 16.0, 400.0);
        assert_eq!(edited_runs.len(), plain.runs.len());
        assert_eq!(edited_runs[0].glyphs.len(), plain.runs[0].glyphs.len());
    }

    fn multiline(font: &mut Font, text: &str, width: Option<f32>) -> TextEditor {
        let mut editor = TextEditor::new(16.0);
        editor.set_width(width);
        font.apply_text_edit(
            &mut editor,
            TextEditOp::InsertOrReplace(text.to_string()),
            FontFamily::SansSerif,
            400.0,
        );
        font.apply_text_edit(
            &mut editor,
            TextEditOp::MoveTextStart,
            FontFamily::SansSerif,
            400.0,
        );
        editor
    }

    fn op(font: &mut Font, editor: &mut TextEditor, op: TextEditOp) -> bool {
        font.apply_text_edit(editor, op, FontFamily::SansSerif, 400.0)
    }

    #[test]
    fn down_and_up_move_between_lines_and_keep_the_column() {
        let mut font = Font::load_embedded();
        let mut editor = multiline(&mut font, "abcdef\nab\nabcdef", None);
        op(&mut font, &mut editor, TextEditOp::MoveToPoint(200.0, 0.0));
        assert_eq!(editor.selection().focus, 6, "end of the first line");
        op(&mut font, &mut editor, TextEditOp::MoveDown);
        assert_eq!(
            editor.selection().focus,
            9,
            "clamped to the short line's end"
        );
        op(&mut font, &mut editor, TextEditOp::MoveDown);
        assert_eq!(
            editor.selection().focus,
            16,
            "the column comes back on the long one"
        );
        op(&mut font, &mut editor, TextEditOp::MoveUp);
        assert_eq!(editor.selection().focus, 9);
    }

    #[test]
    fn a_wrap_width_breaks_a_long_line_and_home_end_stay_on_the_visual_row() {
        let mut font = Font::load_embedded();
        let long = "word ".repeat(12);
        let mut wrapped = multiline(&mut font, long.trim_end(), Some(80.0));
        let mut unwrapped = multiline(&mut font, long.trim_end(), None);
        let (_, wrapped_height) = font.content_size(&mut wrapped);
        let (_, unwrapped_height) = font.content_size(&mut unwrapped);
        assert!(
            wrapped_height > unwrapped_height * 2.0,
            "wrapping adds rows"
        );

        op(&mut font, &mut wrapped, TextEditOp::MoveLineEnd);
        let row_end = wrapped.selection().focus;
        assert!(
            row_end < long.trim_end().len(),
            "End stops at the end of the visual row"
        );
        op(&mut font, &mut wrapped, TextEditOp::MoveDown);
        op(&mut font, &mut wrapped, TextEditOp::MoveLineStart);
        assert!(
            wrapped.selection().focus >= row_end,
            "the next row starts after the first ends"
        );
    }

    #[test]
    fn a_point_op_uses_y_to_pick_the_line() {
        let mut font = Font::load_embedded();
        let mut editor = multiline(&mut font, "first\nsecond\nthird", None);
        let (_, _, _, line_bottom) = font.caret_rect(&mut editor).unwrap();
        op(
            &mut font,
            &mut editor,
            TextEditOp::MoveToPoint(0.0, line_bottom * 1.5),
        );
        assert_eq!(editor.selection().focus, 6, "the start of the second line");
        op(
            &mut font,
            &mut editor,
            TextEditOp::MoveToPoint(0.0, line_bottom * 2.5),
        );
        assert_eq!(editor.selection().focus, 13, "the start of the third line");
    }

    #[test]
    fn select_hard_line_at_point_selects_the_whole_paragraph_even_when_wrapped() {
        let mut font = Font::load_embedded();
        let long = "word ".repeat(12);
        let text = format!("head\n{}\ntail", long.trim_end());
        let mut editor = multiline(&mut font, &text, Some(80.0));
        let (_, _, _, line_bottom) = font.caret_rect(&mut editor).unwrap();
        op(
            &mut font,
            &mut editor,
            TextEditOp::SelectHardLineAtPoint(10.0, line_bottom * 2.5),
        );
        assert_eq!(editor.selected_text(), Some(long.trim_end()));
    }

    #[test]
    fn shift_down_extends_the_selection_across_lines() {
        let mut font = Font::load_embedded();
        let mut editor = multiline(&mut font, "ab\ncd\nef", None);
        op(&mut font, &mut editor, TextEditOp::SelectDown);
        op(&mut font, &mut editor, TextEditOp::SelectDown);
        assert_eq!(editor.selected_text(), Some("ab\ncd\n"));
    }
}
