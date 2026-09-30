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
        font: &mut Font,
    ) {
        let mut states = self.states.borrow_mut();
        let mut seen = HashSet::new();

        let editable_inputs = arena.find_all(|arena, id| {
            arena.tag(id) == "input" && focus::is_editable_input_type(arena.input_type(id))
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

            if let Some(state) = states.get_mut(id) {
                state.font_family = font_family;
                state.font_weight = font_weight;
                state.kind = value_kind(arena, node);
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
                }
            } else {
                let font_size = style.map_or(16.0, |s| s.font_size);
                let mut editor = TextEditor::new(font_size);
                editor.set_text(value);
                font.apply_text_edit(
                    &mut editor,
                    TextEditOp::MoveTextEnd,
                    font_family,
                    font_weight,
                );
                states.insert(
                    id.to_string(),
                    TextInputState {
                        editor,
                        last_committed_text: value.to_string(),
                        kind: value_kind(arena, node),
                        bad_input: false,
                        font_family,
                        font_weight,
                        undo_stack: Vec::new(),
                        redo_stack: Vec::new(),
                        coalescing_insert: false,
                    },
                );
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

        let is_coalescable_insert = matches!(&op, TextEditOp::InsertOrReplace(text) if text.chars().count() == 1)
            && state.editor.selection().is_collapsed();
        let before = (state.editor.text(), state.editor.selection());

        let changed =
            font.apply_text_edit(&mut state.editor, op, state.font_family, state.font_weight);
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
        let state = states.get_mut(id)?;
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
        let raw = state.editor.text();
        let new_text = sanitize_value(state.kind, &raw);
        state.bad_input = !raw.is_empty() && new_text.is_empty() && state.kind == ValueKind::Number;
        state.last_committed_text = new_text.clone();
        Some(new_text)
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
        font.apply_text_edit(
            &mut state.editor,
            TextEditOp::SetCompose(text.to_owned(), cursor),
            state.font_family,
            state.font_weight,
        );
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
        registry.sync(&arena, &styles, &mut font);
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
        registry.sync(&arena, &styles, &mut font);

        // The owner rejected this, so a fresh render's own arena still
        // carries the original "hello" -- exactly what a real rejecting
        // Binding would produce.
        registry.apply("x", TextEditOp::InsertOrReplace("!".to_string()), &mut font);
        let tree: Element = view! { <input type="text" id="x" value={Binding::new("hello".to_string(), |_| {})} /> };
        arena = Arena::build(&tree);
        registry.sync(&arena, &styles, &mut font);

        registry.apply("x", TextEditOp::SelectAll, &mut font);
        assert_eq!(
            registry.selected_text("x"),
            Some("hello".to_string()),
            "a rejected edit must resync back to what the owner actually holds"
        );
    }

    #[test]
    fn sync_drops_state_for_an_input_no_longer_present() {
        let (registry, mut font) = synced_registry("hello");
        let empty: Element = view! { <div /> };
        let arena = Arena::build(&empty);
        let styles: HashMap<NodeId, ComputedStyle> = HashMap::new();
        registry.sync(&arena, &styles, &mut font);
        assert_eq!(registry.apply("x", TextEditOp::SelectAll, &mut font), None);
    }
}
