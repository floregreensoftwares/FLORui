//! Input editing state: caret, selection, and undo/redo for
//! every editable `<input>` (see [`crate::focus::is_editable_input_type`]
//! for which `type`s that means this slice).
//!
//! `<input>` is a raw primitive tag, not a `#[component]` — there is no
//! hook call-site of its own for it to own `use_signal`/`use_attachment`
//! state through. So [`TextInputRegistry`] is synced *structurally*,
//! walking the freshly-built [`Arena`] once per [`crate::UiRuntime::update`]
//! the same way [`crate::focus`]'s own resolution already does, rather
//! than registered by a component call the way [`crate::size_observer`]'s
//! registry is. Keyed by the input's `id`; [`crate::auto_id`] gives one to
//! any input that has none, so a tree that skipped it (a test building an
//! [`Arena`] directly) is the only way to reach the once-only warning in
//! [`TextInputRegistry::sync`].
//!
//! IME composition ([`TextInputRegistry::set_compose`]/[`TextInputRegistry::clear_compose`])
//! deliberately bypasses `undo_stack`/`redo_stack`/`last_committed_text`
//! entirely — a live preedit is real text in the buffer (it must paint),
//! but never a *committed* one; only a real `Ime::Commit` (routed through
//! the ordinary [`TextInputRegistry::apply`] like any other insert) ever
//! reaches those.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use florui_style::{Arena, ComputedStyle, NodeId};
use florui_text::Font;
use florui_text::editing::{ByteSelection, TextEditOp, TextEditor};

use crate::focus;
use crate::form_control::{ValueKind, sanitize_value, value_kind};

/// Real shaped glyphs, a caret rect (`x0,y0,x1,y1`), selection rects, and
/// the current IME preedit area (`None` unless actually composing) — see
/// [`TextInputRegistry::paint_data`].
type TextInputPaintData = (
    Vec<florui_text::ShapedRun>,
    Option<(f32, f32, f32, f32)>,
    Vec<(f32, f32, f32, f32)>,
    Option<(f32, f32, f32, f32)>,
);

/// Bounded so a very long editing session can't grow this without limit —
/// not spec-mandated to an exact number, just "bounded," matching this
/// codebase's own precedent for such constants (see `desktop.rs`'s
/// `WINDOW_STATE_SAVE_DEBOUNCE`'s own doc).
const MAX_UNDO_ENTRIES: usize = 200;

struct UndoEntry {
    text: String,
    selection: ByteSelection,
}

struct TextInputState {
    editor: TextEditor,
    /// What this input's owner is believed to currently hold — set
    /// optimistically the instant a `request_update` is issued, *not*
    /// only once the owner is confirmed to have accepted it. The next
    /// [`TextInputRegistry::sync`] compares this against the arena's
    /// fresh `value` attribute: a match means the request was accepted
    /// (or nothing else changed the owner's value); a mismatch means it
    /// was rejected (or the owner changed independently) and the buffer
    /// resyncs to whatever the owner actually holds.
    last_committed_text: String,
    /// How typed text becomes the value handed to the owner: the buffer
    /// keeps what was typed, the owner gets the sanitized form.
    kind: ValueKind,
    /// Typed text that is not a number at all (`1e`, `-`): the value is empty
    /// but the field is not, which is what `badInput` reports.
    bad_input: bool,
    /// Refreshed every [`TextInputRegistry::sync`] from the current
    /// render's own `ComputedStyle` — [`TextInputRegistry::apply`]/
    /// `undo`/`redo` all reshape with whatever this input's real font
    /// currently is, without their own callers needing to know style
    /// exists.
    font_family: florui_text::FontFamily,
    font_weight: f32,
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    /// Whether `undo_stack`'s top entry is a valid restore point for
    /// *continuing* a coalesced run of single-character insertions — see
    /// [`TextInputRegistry::apply`]'s own doc for the coalescing rule.
    coalescing_insert: bool,
    /// A `<textarea>`: wraps at its content width and scrolls vertically.
    multiline: bool,
    /// A single-line field that scrolls sideways to keep its caret in view.
    scrolls_x: bool,
    /// A password field shows one bullet per character, so its horizontal
    /// geometry is this advance times the character count.
    mask_advance: Option<f32>,
    /// A `readonly` field can be focused, selected and copied but never
    /// edited.
    read_only: bool,
    /// `maxlength` in UTF-16 units, for the fields that enforce it while the
    /// user types.
    max_length: Option<usize>,
    /// How far the text is scrolled, in logical pixels.
    scroll: (f32, f32),
    /// The content box in logical pixels, what the text scrolls inside.
    viewport: (f32, f32),
}

/// Width a vertical scrollbar takes from a textarea's text once the
/// content overflows (measured in Edge on Windows).
pub(crate) const SCROLLBAR_WIDTH: f32 = 15.0;

impl TextInputState {
    /// Wraps a multiline field at the viewport width, giving up the
    /// scrollbar's width once the text is taller than the viewport, then
    /// keeps the scroll offset inside the new bounds.
    fn relayout(&mut self, font: &mut Font) {
        if !self.multiline {
            return;
        }
        let (vw, vh) = self.viewport;
        self.editor.set_width(Some(vw.max(1.0)));
        let (_, height) = font.content_size(&mut self.editor);
        if height > vh {
            self.editor.set_width(Some((vw - SCROLLBAR_WIDTH).max(1.0)));
        }
        self.clamp_scroll(font);
    }

    /// The width of what is shown and, if there is one, the caret's x extent.
    fn shown_extent(&mut self, font: &mut Font) -> (f32, Option<(f32, f32)>) {
        let Some(advance) = self.mask_advance else {
            let (width, _) = font.content_size(&mut self.editor);
            let caret = font
                .caret_rect(&mut self.editor)
                .map(|(x0, _, x1, _)| (x0, x1));
            return (width, caret);
        };
        let text = self.editor.text();
        let focus = self.editor.selection().focus.min(text.len());
        let before = text.get(..focus).map_or(0, |s| s.chars().count());
        let x = before as f32 * advance;
        (text.chars().count() as f32 * advance, Some((x, x + 1.0)))
    }

    fn clamp_scroll(&mut self, font: &mut Font) {
        let (_, content_h) = font.content_size(&mut self.editor);
        let content_w = self.shown_extent(font).0;
        let max_x = if self.scrolls_x {
            (content_w - self.viewport.0).max(0.0)
        } else {
            0.0
        };
        let max_y = if self.multiline {
            (content_h - self.viewport.1).max(0.0)
        } else {
            0.0
        };
        self.scroll = (
            self.scroll.0.clamp(0.0, max_x),
            self.scroll.1.clamp(0.0, max_y),
        );
    }

    /// Scrolls the least distance that brings the caret into view.
    fn reveal_caret(&mut self, font: &mut Font) {
        if !self.multiline && !self.scrolls_x {
            return;
        }
        self.clamp_scroll(font);
        let Some((_, y0, _, y1)) = font.caret_rect(&mut self.editor) else {
            return;
        };
        let (x0, x1) = self.shown_extent(font).1.unwrap_or_default();
        let (vw, vh) = self.viewport;
        if self.multiline {
            if y0 < self.scroll.1 {
                self.scroll.1 = y0;
            } else if y1 > self.scroll.1 + vh {
                self.scroll.1 = y1 - vh;
            }
        }
        if self.scrolls_x {
            if x0 < self.scroll.0 {
                self.scroll.0 = x0;
            } else if x1 + 1.0 > self.scroll.0 + vw {
                self.scroll.0 = x1 + 1.0 - vw;
            }
        }
        self.clamp_scroll(font);
    }
}

/// Whether `node` is a single-line text field whose text scrolls sideways.
fn scrolls_sideways(arena: &Arena, node: NodeId) -> bool {
    arena.tag(node) == "input"
}

/// The bullet advance of a password field, whose shown text is a mask.
fn mask_advance(
    arena: &Arena,
    node: NodeId,
    font: &mut Font,
    style: Option<&ComputedStyle>,
) -> Option<f32> {
    if arena.input_type(node) != Some("password") {
        return None;
    }
    let family = style.map_or(florui_text::FontFamily::SansSerif, |s| {
        florui_layout::to_text_font_family(s.font_family)
    });
    let (size, weight) = style.map_or((16.0, 400.0), |s| (s.font_size, s.font_weight));
    Some(font.measure(family, "\u{2022}", size, weight).width)
}

/// `maxlength` for a text field that enforces it as the user types (every
/// text-like input and a textarea, but not a number field).
fn max_length(arena: &Arena, node: NodeId) -> Option<usize> {
    let enforced = arena.tag(node) == "textarea"
        || matches!(
            arena.input_type(node),
            None | Some("text") | Some("password") | Some("email")
        );
    enforced
        .then(|| arena.attr(node, "maxlength"))
        .flatten()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
}

/// `op` cut to what still fits under `maxlength`: typing past the limit is
/// blocked and pasted text is cut to fit (measured in Edge, where a line
/// break counts as one). `None` when nothing of an insertion fits.
fn limit_insertion(state: &TextInputState, op: TextEditOp) -> Option<TextEditOp> {
    let (Some(max), TextEditOp::InsertOrReplace(text)) = (state.max_length, &op) else {
        return Some(op);
    };
    let units = |s: &str| s.encode_utf16().count();
    let current = units(&state.editor.text());
    let selected = state.editor.selected_text().map_or(0, units);
    let room = max.saturating_sub(current - selected);
    let mut kept = String::new();
    let mut used = 0;
    for c in text.chars() {
        let width = c.len_utf16();
        if used + width > room {
            break;
        }
        kept.push(c);
        used += width;
    }
    if kept.is_empty() && !text.is_empty() {
        return None;
    }
    Some(TextEditOp::InsertOrReplace(kept))
}

/// Whether `op` would change the text, as opposed to only moving the caret
/// or the selection.
fn changes_text(op: &TextEditOp) -> bool {
    matches!(
        op,
        TextEditOp::InsertOrReplace(_)
            | TextEditOp::Delete
            | TextEditOp::Backdelete
            | TextEditOp::DeleteWord
            | TextEditOp::BackdeleteWord
            | TextEditOp::SetCompose(..)
    )
}

/// The content box of a node (its layout box less border and padding) in
/// logical pixels.
fn content_box(
    style: Option<&ComputedStyle>,
    layout: Option<&florui_layout::BoxLayout>,
) -> (f32, f32) {
    let (Some(style), Some(layout)) = (style, layout) else {
        return (0.0, 0.0);
    };
    let width = layout.width
        - style.border.left.width
        - style.border.right.width
        - style.padding.left
        - style.padding.right;
    let height = layout.height
        - style.border.top.width
        - style.border.bottom.width
        - style.padding.top
        - style.padding.bottom;
    (width.max(0.0), height.max(0.0))
}

/// Real editing state for every currently-live editable `<input>`, keyed
/// by its own `id` attribute — see this module's own doc for why a
/// structural per-render sync, not a hook, populates it.
#[derive(Default)]
pub struct TextInputRegistry {
    states: RefCell<HashMap<String, TextInputState>>,
    warned_missing_id: Cell<bool>,
}

impl TextInputRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called once per [`crate::UiRuntime::update`], after this render's
    /// own style/layout exist. Creates state for a newly-seen editable
    /// input, drops state for one no longer present (matching
    /// `resolve_focus`'s own "clears if no longer present" precedent),
    /// and resyncs a mismatched buffer — see [`TextInputState::last_committed_text`]'s
    /// own doc for exactly what "mismatched" means and why.
    pub(crate) fn sync(
        &self,
        arena: &Arena,
        styles: &HashMap<NodeId, ComputedStyle>,
        layouts: &HashMap<NodeId, florui_layout::BoxLayout>,
        font: &mut Font,
    ) {
        let mut states = self.states.borrow_mut();
        let mut seen = HashSet::new();

        let editable_inputs = arena.find_all(|arena, id| {
            arena.tag(id) == "textarea"
                || (arena.tag(id) == "input" && focus::is_editable_input_type(arena.input_type(id)))
        });
        for node in editable_inputs {
            let Some(id) = arena.id_attr(node) else {
                if !self.warned_missing_id.replace(true) {
                    eprintln!(
                        "florui-platform: an editable <input> has no id attribute -- it gets no \
                         caret/selection/undo/focus behavior until it has one (this warning \
                         prints only once)"
                    );
                }
                continue;
            };
            seen.insert(id.to_string());
            let value = arena.value_attr(node).unwrap_or_default();
            let style = styles.get(&node);
            let font_family = style.map_or(florui_text::FontFamily::SansSerif, |s| {
                florui_layout::to_text_font_family(s.font_family)
            });
            let font_weight = style.map_or(400.0, |s| s.font_weight);
            let multiline = arena.tag(node) == "textarea";
            let viewport = content_box(style, layouts.get(&node));

            if let Some(state) = states.get_mut(id) {
                state.font_family = font_family;
                state.font_weight = font_weight;
                state.kind = value_kind(arena, node);
                state.read_only = arena.attr_flag(node, "readonly");
                state.max_length = max_length(arena, node);
                let mut needs_relayout = state.viewport != viewport;
                state.viewport = viewport;
                state.scrolls_x = scrolls_sideways(arena, node);
                state.mask_advance = mask_advance(arena, node, font, style);
                if state.last_committed_text != value {
                    state.editor.set_text(value);
                    state.bad_input = false;
                    font.apply_text_edit(
                        &mut state.editor,
                        TextEditOp::MoveTextEnd,
                        font_family,
                        font_weight,
                    );
                    state.last_committed_text = value.to_string();
                    state.undo_stack.clear();
                    state.redo_stack.clear();
                    state.coalescing_insert = false;
                    needs_relayout = true;
                }
                if needs_relayout {
                    state.relayout(font);
                    state.reveal_caret(font);
                }
            } else {
                let font_size = style.map_or(16.0, |s| s.font_size);
                let mut editor = TextEditor::new(font_size);
                editor.set_text(value);
                // A textarea starts with its caret at the start (measured in
                // Edge); a single-line field's is at the end.
                font.apply_text_edit(
                    &mut editor,
                    if multiline {
                        TextEditOp::MoveTextStart
                    } else {
                        TextEditOp::MoveTextEnd
                    },
                    font_family,
                    font_weight,
                );
                let mut state = TextInputState {
                    editor,
                    last_committed_text: value.to_string(),
                    kind: value_kind(arena, node),
                    bad_input: false,
                    font_family,
                    font_weight,
                    undo_stack: Vec::new(),
                    redo_stack: Vec::new(),
                    coalescing_insert: false,
                    multiline,
                    scrolls_x: scrolls_sideways(arena, node),
                    mask_advance: mask_advance(arena, node, font, style),
                    read_only: arena.attr_flag(node, "readonly"),
                    max_length: max_length(arena, node),
                    scroll: (0.0, 0.0),
                    viewport,
                };
                state.relayout(font);
                state.reveal_caret(font);
                states.insert(id.to_string(), state);
            }
        }

        states.retain(|id, _| seen.contains(id));
    }

    /// Applies one edit/movement/selection op to `id`'s editor. Returns
    /// the new text if the buffer actually changed — the caller's cue to
    /// `request_update` it through the node's `Binding` — `None` for a
    /// pure movement/selection op, or if `id` has no tracked state (not
    /// currently focused/synced, or missing its own `id` attribute).
    ///
    /// Undo coalescing: consecutive single-character `InsertOrReplace`
    /// ops with no active selection collapse into one undo step (one
    /// per typing burst, not per keystroke) — any other op (a delete, a
    /// paste, a multi-character insert, a movement that turns out to
    /// change nothing) always starts a fresh entry. Any change clears
    /// the redo stack, standard undo/redo semantics.
    pub fn apply(&self, id: &str, op: TextEditOp, font: &mut Font) -> Option<String> {
        let mut states = self.states.borrow_mut();
        let state = states.get_mut(id)?;
        if state.read_only && changes_text(&op) {
            return None;
        }
        let op = limit_insertion(state, op)?;

        let is_coalescable_insert = matches!(&op, TextEditOp::InsertOrReplace(text) if text.chars().count() == 1)
            && state.editor.selection().is_collapsed();
        let before = (state.editor.text(), state.editor.selection());

        let changed =
            font.apply_text_edit(&mut state.editor, op, state.font_family, state.font_weight);
        if changed {
            state.relayout(font);
        }
        state.reveal_caret(font);
        if !changed {
            return None;
        }

        if !(state.coalescing_insert && is_coalescable_insert) {
            state.undo_stack.push(UndoEntry {
                text: before.0,
                selection: before.1,
            });
            if state.undo_stack.len() > MAX_UNDO_ENTRIES {
                state.undo_stack.remove(0);
            }
        }
        state.coalescing_insert = is_coalescable_insert;
        state.redo_stack.clear();

        let raw = state.editor.text();
        let new_text = sanitize_value(state.kind, &raw);
        state.bad_input = !raw.is_empty() && new_text.is_empty() && state.kind == ValueKind::Number;
        state.last_committed_text = new_text.clone();
        Some(new_text)
    }

    /// Pops the most recent undo entry and restores it, pushing the
    /// pre-undo state onto the redo stack — `None` if `id` has no tracked
    /// state or nothing left to undo.
    pub fn undo(&self, id: &str, font: &mut Font) -> Option<String> {
        Self::restore(&mut self.states.borrow_mut(), id, font, true)
    }

    /// Same as [`Self::undo`], symmetrically, off the redo stack.
    pub fn redo(&self, id: &str, font: &mut Font) -> Option<String> {
        Self::restore(&mut self.states.borrow_mut(), id, font, false)
    }

    fn restore(
        states: &mut HashMap<String, TextInputState>,
        id: &str,
        font: &mut Font,
        is_undo: bool,
    ) -> Option<String> {
        let state = states.get_mut(id).filter(|state| !state.read_only)?;
        let entry = if is_undo {
            state.undo_stack.pop()?
        } else {
            state.redo_stack.pop()?
        };
        let current = UndoEntry {
            text: state.editor.text(),
            selection: state.editor.selection(),
        };
        if is_undo {
            state.redo_stack.push(current);
        } else {
            state.undo_stack.push(current);
        }

        state.editor.set_text(&entry.text);
        font.apply_text_edit(
            &mut state.editor,
            TextEditOp::SelectByteRange(entry.selection.anchor, entry.selection.focus),
            state.font_family,
            state.font_weight,
        );
        state.coalescing_insert = false;
        state.relayout(font);
        state.reveal_caret(font);
        let raw = state.editor.text();
        let new_text = sanitize_value(state.kind, &raw);
        state.bad_input = !raw.is_empty() && new_text.is_empty() && state.kind == ValueKind::Number;
        state.last_committed_text = new_text.clone();
        Some(new_text)
    }

    /// `id`'s content box in logical pixels, `(0, 0)` for an untracked id.
    pub(crate) fn viewport(&self, id: &str) -> (f32, f32) {
        self.states
            .borrow()
            .get(id)
            .map_or((0.0, 0.0), |state| state.viewport)
    }

    /// `id`'s scroll offset in logical pixels, `(0, 0)` for an untracked id.
    pub(crate) fn scroll_offset(&self, id: &str) -> (f32, f32) {
        self.states
            .borrow()
            .get(id)
            .map_or((0.0, 0.0), |state| state.scroll)
    }

    /// A single-line field shows its start again once it loses focus
    /// (measured in Edge); a textarea keeps where it was scrolled to.
    pub(crate) fn reset_scroll_on_blur(&self, id: &str) {
        if let Some(state) = self.states.borrow_mut().get_mut(id)
            && !state.multiline
        {
            state.scroll.0 = 0.0;
        }
    }

    /// The real-text x that a click at `x` in a password field's bullet
    /// space lands on: the nearest bullet boundary, mapped to that
    /// character's own position. Any other field returns `x` unchanged.
    pub(crate) fn unmask_x(&self, id: &str, x: f32, font: &mut Font) -> f32 {
        let mut states = self.states.borrow_mut();
        let Some(state) = states.get_mut(id) else {
            return x;
        };
        let Some(advance) = state.mask_advance.filter(|a| *a > 0.0) else {
            return x;
        };
        let glyph_x: Vec<f32> = font
            .shaped_runs_for_edit(&mut state.editor)
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.x))
            .collect();
        let index = ((x / advance).round().max(0.0) as usize).min(glyph_x.len());
        glyph_x
            .get(index)
            .copied()
            .unwrap_or_else(|| font.content_size(&mut state.editor).0)
    }

    /// Scrolls a multiline field by `dy` logical pixels; `true` if it moved,
    /// `false` at a limit, so the wheel can pass on to whatever scrolls
    /// behind it, as a browser does.
    pub(crate) fn scroll_by(&self, id: &str, dy: f32, font: &mut Font) -> bool {
        let mut states = self.states.borrow_mut();
        let Some(state) = states.get_mut(id).filter(|state| state.multiline) else {
            return false;
        };
        let before = state.scroll.1;
        state.scroll.1 += dy;
        state.clamp_scroll(font);
        state.scroll.1 != before
    }

    /// One page up or down: the caret moves by the field's height less one
    /// line (measured in Edge: three rows in a four-row field), keeping its
    /// column, and the view follows it. `extend` grows the selection.
    pub(crate) fn page(
        &self,
        id: &str,
        direction: i32,
        extend: bool,
        font: &mut Font,
    ) -> Option<String> {
        let (x, y, step) = {
            let mut states = self.states.borrow_mut();
            let state = states.get_mut(id).filter(|state| state.multiline)?;
            let (x0, y0, _, y1) = font.caret_rect(&mut state.editor)?;
            let line = y1 - y0;
            (x0, (y0 + y1) / 2.0, (state.viewport.1 - line).max(line))
        };
        let target = (x, y + direction as f32 * step);
        let op = if extend {
            TextEditOp::ExtendSelectionToPoint(target.0, target.1)
        } else {
            TextEditOp::MoveToPoint(target.0, target.1)
        };
        self.apply(id, op, font)
    }

    /// Whether `id`'s typed text is not a number, for `badInput`.
    pub(crate) fn is_bad_input(&self, id: &str) -> bool {
        self.states
            .borrow()
            .get(id)
            .is_some_and(|state| state.bad_input)
    }

    /// The currently selected text for `id`, if any — `None` both for a
    /// collapsed selection (nothing to copy/cut) and for an untracked
    /// `id`.
    pub fn selected_text(&self, id: &str) -> Option<String> {
        self.states
            .borrow()
            .get(id)?
            .editor
            .selected_text()
            .map(str::to_owned)
    }

    /// The real shaped glyphs plus caret/selection/compose geometry
    /// `id`'s editor currently has — everything `crate::desktop`'s own
    /// `redraw` needs to build one `florui_paint::TextInputPaint` entry.
    /// `None` for an untracked `id`. Password masking isn't decided here
    /// (this registry doesn't track `type`) — the caller substitutes
    /// glyph ids afterward if the node's own `type` calls for it.
    pub(crate) fn paint_data(&self, id: &str, font: &mut Font) -> Option<TextInputPaintData> {
        let mut states = self.states.borrow_mut();
        let state = states.get_mut(id)?;
        let runs = font.shaped_runs_for_edit(&mut state.editor);
        let caret_rect = font.caret_rect(&mut state.editor);
        let selection_rects = font.selection_rects(&mut state.editor);
        let compose_rect = state
            .editor
            .is_composing()
            .then(|| font.ime_cursor_area(&mut state.editor));
        Some((runs, caret_rect, selection_rects, compose_rect))
    }

    /// Starts or updates `id`'s IME preedit composition — real text in
    /// the buffer (so it paints, and so [`Self::paint_data`]'s geometry
    /// reflects it), but deliberately untouched by undo/redo or
    /// [`TextInputState::last_committed_text`]: composing-so-far text is
    /// never itself a final value (see this module's own doc). A no-op
    /// for an untracked `id` (not currently focused/synced, or missing
    /// its own `id` attribute).
    ///
    /// `text` empty routes to [`Self::clear_compose`] instead of
    /// forwarding an empty string to Parley, which panics on one in
    /// debug builds — winit's own contract sends exactly this (an empty
    /// `Ime::Preedit`) immediately before every `Ime::Commit`.
    pub fn set_compose(
        &self,
        id: &str,
        text: &str,
        cursor: Option<(usize, usize)>,
        font: &mut Font,
    ) {
        if text.is_empty() {
            self.clear_compose(id, font);
            return;
        }
        let mut states = self.states.borrow_mut();
        let Some(state) = states.get_mut(id) else {
            return;
        };
        if state.read_only {
            return;
        }
        font.apply_text_edit(
            &mut state.editor,
            TextEditOp::SetCompose(text.to_owned(), cursor),
            state.font_family,
            state.font_weight,
        );
        state.reveal_caret(font);
    }

    /// Ends `id`'s composition, if any — a real no-op when nothing is
    /// composing (Parley's own `clear_compose` contract), so safe to
    /// call defensively (e.g. a text input losing focus mid-composition,
    /// which winit itself never signals via `Ime::Disabled` since the
    /// newly-focused input keeps IME allowed too).
    pub fn clear_compose(&self, id: &str, font: &mut Font) {
        let mut states = self.states.borrow_mut();
        let Some(state) = states.get_mut(id) else {
            return;
        };
        font.apply_text_edit(
            &mut state.editor,
            TextEditOp::ClearCompose,
            state.font_family,
            state.font_weight,
        );
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;

    fn synced_registry(value: &str) -> (TextInputRegistry, Font) {
        let binding = Binding::new(value.to_string(), |_| {});
        let tree: Element = view! { <input type="text" id="x" value={binding} /> };
        let arena = Arena::build(&tree);
        let mut font = Font::load_embedded();
        let styles: HashMap<NodeId, ComputedStyle> = HashMap::new();
        let registry = TextInputRegistry::new();
        registry.sync(&arena, &styles, &HashMap::new(), &mut font);
        (registry, font)
    }

    #[test]
    fn sync_creates_state_seeded_with_the_arenas_current_value() {
        let (registry, mut font) = synced_registry("hello");
        assert_eq!(registry.selected_text("x"), None);
        registry.apply("x", TextEditOp::SelectAll, &mut font);
        assert_eq!(registry.selected_text("x"), Some("hello".to_string()));
    }

    #[test]
    fn apply_returns_the_new_text_only_when_it_actually_changed() {
        let (registry, mut font) = synced_registry("hi");
        assert_eq!(registry.apply("x", TextEditOp::MoveLeft, &mut font), None);
        // Undoes the MoveLeft above, back to the end -- otherwise the
        // insert below would land between "h" and "i", not at the end.
        assert_eq!(registry.apply("x", TextEditOp::MoveRight, &mut font), None);
        assert_eq!(
            registry.apply("x", TextEditOp::InsertOrReplace("!".to_string()), &mut font),
            Some("hi!".to_string())
        );
    }

    #[test]
    fn consecutive_single_character_inserts_coalesce_into_one_undo_step() {
        let (registry, mut font) = synced_registry("");
        for ch in ["a", "b", "c"] {
            registry.apply("x", TextEditOp::InsertOrReplace(ch.to_string()), &mut font);
        }
        assert_eq!(
            registry.undo("x", &mut font),
            Some(String::new()),
            "one undo must remove the whole typed burst, not one character"
        );
    }

    #[test]
    fn a_delete_does_not_coalesce_with_a_preceding_insert() {
        let (registry, mut font) = synced_registry("");
        registry.apply("x", TextEditOp::InsertOrReplace("a".to_string()), &mut font);
        registry.apply("x", TextEditOp::Backdelete, &mut font);
        assert_eq!(
            registry.undo("x", &mut font),
            Some("a".to_string()),
            "undoing the delete alone must restore just the deleted character"
        );
        assert_eq!(registry.undo("x", &mut font), Some(String::new()));
    }

    #[test]
    fn undo_then_redo_restores_the_exact_prior_selection() {
        let (registry, mut font) = synced_registry("hello");
        // Selects "el" (bytes 1..3) and replaces it with "X" -- "hello" ->
        // "hXlo", not just "hX"; the whole point of this test is that
        // undo/redo round-trips the *real* text exactly, so getting the
        // expected value right here matters.
        registry.apply("x", TextEditOp::SelectByteRange(1, 3), &mut font);
        registry.apply("x", TextEditOp::InsertOrReplace("X".to_string()), &mut font);
        registry.undo("x", &mut font);
        registry.apply("x", TextEditOp::SelectAll, &mut font);
        assert_eq!(
            registry.selected_text("x"),
            Some("hello".to_string()),
            "undo must restore the full original text"
        );

        registry.redo("x", &mut font);
        registry.apply("x", TextEditOp::SelectAll, &mut font);
        assert_eq!(registry.selected_text("x"), Some("hXlo".to_string()));
    }

    #[test]
    fn a_rejected_owner_update_resyncs_the_buffer_on_the_next_sync() {
        let binding = Binding::new("hello".to_string(), |_| {}); // always rejects
        let tree: Element = view! { <input type="text" id="x" value={binding} /> };
        let mut arena = Arena::build(&tree);
        let mut font = Font::load_embedded();
        let styles: HashMap<NodeId, ComputedStyle> = HashMap::new();
        let registry = TextInputRegistry::new();
        registry.sync(&arena, &styles, &HashMap::new(), &mut font);

        // The owner rejected this, so a fresh render's own arena still
        // carries the original "hello" -- exactly what a real rejecting
        // Binding would produce.
        registry.apply("x", TextEditOp::InsertOrReplace("!".to_string()), &mut font);
        let tree: Element = view! { <input type="text" id="x" value={Binding::new("hello".to_string(), |_| {})} /> };
        arena = Arena::build(&tree);
        registry.sync(&arena, &styles, &HashMap::new(), &mut font);

        registry.apply("x", TextEditOp::SelectAll, &mut font);
        assert_eq!(
            registry.selected_text("x"),
            Some("hello".to_string()),
            "a rejected edit must resync back to what the owner actually holds"
        );
    }

    fn typed_field_scroll(kind: &str, typed: usize) -> ((f32, f32), f32) {
        use taffy::prelude::*;
        let tree: Element = match kind {
            "password" => view! { <input type="password" id="x" class="f" value="" /> },
            _ => view! { <input type="text" id="x" class="f" value="" /> },
        };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(
            ".f { width: 100px; height: 24px; padding: 0px; border: none; }",
        )
        .unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &florui_style::InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        let registry = TextInputRegistry::new();
        registry.sync(&arena, &styles, &layouts, &mut font);
        for _ in 0..typed {
            registry.apply("x", TextEditOp::InsertOrReplace("x".to_string()), &mut font);
        }
        let advance = mask_advance(
            &arena,
            arena.roots()[0],
            &mut font,
            styles.get(&arena.roots()[0]),
        )
        .unwrap_or_default();
        (registry.scroll_offset("x"), advance)
    }

    fn field_registry(kind: &str, value: &str, width: u32) -> (TextInputRegistry, Font, f32) {
        use taffy::prelude::*;
        let tree: Element = match kind {
            "password" => view! { <input type="password" id="x" class="f" /> },
            _ => view! { <input type="text" id="x" class="f" /> },
        };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(&format!(
            ".f {{ width: {width}px; height: 24px; padding: 0px; border: none; }}"
        ))
        .unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &florui_style::InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        let registry = TextInputRegistry::new();
        registry.sync(&arena, &styles, &layouts, &mut font);
        registry.apply(
            "x",
            TextEditOp::InsertOrReplace(value.to_string()),
            &mut font,
        );
        let node = arena.roots()[0];
        let advance = mask_advance(&arena, node, &mut font, styles.get(&node)).unwrap_or_default();
        (registry, font, advance)
    }

    #[test]
    fn a_click_in_a_password_lands_on_the_bullet_it_visibly_hit() {
        let (registry, mut font, advance) = field_registry("password", "iiWWii", 200);
        let glyphs: Vec<f32> = {
            let mut states = registry.states.borrow_mut();
            let state = states.get_mut("x").unwrap();
            font.shaped_runs_for_edit(&mut state.editor)
                .iter()
                .flat_map(|run| run.glyphs.iter().map(|g| g.x))
                .collect()
        };
        // Just past the third bullet's left edge, closer to it than the next.
        let x = registry.unmask_x("x", 2.0 * advance + 0.3 * advance, &mut font);
        assert_eq!(x, glyphs[2], "the third character, by bullet spacing");
        let end = registry.unmask_x("x", 500.0, &mut font);
        assert!(
            end > glyphs[5],
            "past the last bullet is the end of the text"
        );
        let (plain, mut font, _) = field_registry("text", "iiWWii", 200);
        assert_eq!(
            plain.unmask_x("x", 12.5, &mut font),
            12.5,
            "text is untouched"
        );
    }

    #[test]
    fn a_single_line_field_shows_its_start_after_losing_focus() {
        let (registry, _font, _) = field_registry("text", &"x".repeat(80), 60);
        assert!(registry.scroll_offset("x").0 > 0.0);
        registry.reset_scroll_on_blur("x");
        assert_eq!(registry.scroll_offset("x").0, 0.0);
    }
    #[test]
    fn a_password_field_scrolls_by_its_bullets_to_keep_the_caret_in_view() {
        assert_eq!(
            typed_field_scroll("password", 5).0,
            (0.0, 0.0),
            "fits: no scroll"
        );
        let ((x, _), advance) = typed_field_scroll("password", 50);
        assert!(
            (x - (50.0 * advance - 100.0)).abs() < 0.01,
            "scrolled to the end of the bullets: {x}, advance {advance}"
        );
    }
}
