//! `<form>` submission, reset and pseudo-class state on [`UiRuntime`].

use florui_style::{FocusPath, FormState, NodeId};

use super::{UiRuntime, ValidationBubble};
use crate::form;
use crate::form_control::{ButtonKind, FormContext, button_kind};
use crate::validation_message::validation_message;

/// What Enter in a text field does to its form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImplicitSubmit {
    /// Click the form's default submit button.
    Click(NodeId),
    /// Submit with no submitter: the form has no submit button and this
    /// is its only text field.
    Submit(NodeId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubmitOutcome {
    Submitted,
    /// A constraint failed: the first invalid control was focused and
    /// nothing was reported.
    Blocked,
}

impl UiRuntime {
    pub(super) fn with_form_context<R>(&self, f: impl FnOnce(&FormContext) -> R) -> R {
        let edited = self.edited.borrow();
        f(&FormContext {
            arena: &self.arena,
            options: &self.option_summaries,
            edited: &edited,
            text_inputs: &self.text_input_registry,
        })
    }

    pub(super) fn form_state_of(&self, node: NodeId) -> Option<FormState> {
        let validated = self
            .user_validated
            .contains(&FocusPath::of(&self.arena, node));
        self.with_form_context(|ctx| form::form_state(ctx, node, validated))
    }

    /// Validates `form` unless the form or `submitter` opts out, then
    /// reports its data through `onsubmit`. A failed validation focuses
    /// the first invalid control instead, as a browser does.
    pub(crate) fn submit_form(
        &mut self,
        form: NodeId,
        submitter: Option<NodeId>,
        via_keyboard: bool,
    ) -> SubmitOutcome {
        if !form::skips_validation(&self.arena, form, submitter) {
            let invalid = self.with_form_context(|ctx| form::invalid_controls(ctx, form));
            if !invalid.is_empty() {
                for node in form::controls(&self.arena, form) {
                    self.user_validated.insert(FocusPath::of(&self.arena, node));
                }
                for &node in &invalid {
                    self.dispatch_event(node, "invalid");
                }
                let target = invalid
                    .into_iter()
                    .find(|&node| crate::focus::is_focusable(&self.arena, node));
                if !self.set_focused(target, via_keyboard) {
                    self.rebuild_interaction();
                }
                if let Some(target) = target {
                    self.show_validation_bubble(target);
                }
                return SubmitOutcome::Blocked;
            }
        }
        let data = self.with_form_context(|ctx| form::form_data(ctx, form, submitter));
        if let Some(handler) = self.arena.submit_handler(form).cloned() {
            florui_reactive::batch(|| handler.call(data));
        }
        SubmitOutcome::Submitted
    }

    /// `text` as the field at `node` accepts it typed, pasted or set by an
    /// assistive technology. A `<textarea>` keeps line breaks (a lone `\r`
    /// or `\r\n` becomes `\n`); every single-line field drops `\n`, `\r`
    /// and `\t`, as browsers do, before its own type filter runs.
    pub(crate) fn clean_typed(&self, node: NodeId, text: &str) -> String {
        if self.arena.tag(node) == "textarea" {
            return text.replace("\r\n", "\n").replace('\r', "\n");
        }
        let single_line: String = text
            .chars()
            .filter(|c| !matches!(c, '\n' | '\r' | '\t'))
            .collect();
        self.filter_typed(node, &single_line)
    }

    /// `text` as the field at `node` accepts it typed or pasted: a
    /// `type=number` field drops everything but digits, sign, point and
    /// exponent letters.
    pub(crate) fn filter_typed(&self, node: NodeId, text: &str) -> String {
        crate::form_control::filter_typed_text(
            crate::form_control::value_kind(&self.arena, node),
            text,
        )
    }

    /// The value one arrow-key step from the number field at `node`, or
    /// `None` when `node` isn't an editable number field or is already at
    /// its limit.
    pub(crate) fn step_number_value(&self, node: NodeId, direction: i32) -> Option<String> {
        let arena = &self.arena;
        if arena.tag(node) != "input"
            || arena.input_type(node) != Some("number")
            || arena.is_disabled(node)
            || arena.attr_flag(node, "readonly")
        {
            return None;
        }
        let current = arena.value_attr(node).unwrap_or("");
        crate::form_control::stepped_number(arena, node, current, direction)
    }

    /// Shows the validation message of `node` in a bubble under it.
    fn show_validation_bubble(&mut self, node: NodeId) {
        let Some(message) = self.with_form_context(|ctx| validation_message(ctx, node)) else {
            return;
        };
        *self.validation_bubble.borrow_mut() = Some(ValidationBubble {
            field: FocusPath::of(&self.arena, node),
            message,
        });
    }

    /// Remembers that the user resized the textarea `id` to this content
    /// box; it keeps that size from the next render on.
    pub(crate) fn set_resized(&self, id: &str, content_size: (f32, f32)) {
        self.resized
            .borrow_mut()
            .insert(id.to_string(), content_size);
    }

    /// Hides the validation bubble; `true` if one was showing, so the caller
    /// knows a redraw is due.
    pub(crate) fn dismiss_validation_bubble(&self) -> bool {
        self.validation_bubble.borrow_mut().take().is_some()
    }

    /// Hides the bubble if it hangs from `node`.
    pub(super) fn dismiss_validation_bubble_of(&self, node: NodeId) {
        let path = FocusPath::of(&self.arena, node);
        let mut bubble = self.validation_bubble.borrow_mut();
        if bubble.as_ref().is_some_and(|shown| shown.field == path) {
            *bubble = None;
        }
    }

    /// The control the bubble hangs from, in the current arena, if any.
    pub(crate) fn validation_bubble_field(&self) -> Option<NodeId> {
        let bubble = self.validation_bubble.borrow();
        let field = &bubble.as_ref()?.field;
        let candidates = self.arena.find_all(|_, _| true);
        field.resolve(&self.arena, &candidates)
    }

    /// A control that left the tree takes its bubble with it.
    pub(super) fn drop_validation_bubble_if_orphaned(&mut self) {
        if self.validation_bubble.borrow().is_some() && self.validation_bubble_field().is_none() {
            *self.validation_bubble.borrow_mut() = None;
        }
    }

    pub(super) fn position_validation_bubble(&mut self, width: f32, height: f32) {
        if let Some(field) = self.validation_bubble_field() {
            crate::validation_bubble::place(&self.arena, &mut self.layouts, field, (width, height));
        }
    }

    /// The window lost focus: like a browser, the focused control blurs, so
    /// an edited one may now match `:user-invalid`. Focus itself stays put.
    pub(crate) fn window_focus_lost(&mut self) {
        if let Some(path) = self.focused_path.clone()
            && self.edited.borrow().contains(&path)
        {
            self.user_validated.insert(path);
            self.rebuild_interaction();
        }
    }

    /// Reports a reset through `onreset` and forgets what the user has
    /// edited in `form`'s controls. The app resets its own state; nothing
    /// here rewrites a control.
    pub(crate) fn reset_form(&mut self, form: NodeId) {
        self.dispatch_event(form, "reset");
        for node in form::controls(&self.arena, form) {
            let path = FocusPath::of(&self.arena, node);
            self.edited.borrow_mut().remove(&path);
            self.user_validated.remove(&path);
        }
        self.rebuild_interaction();
    }

    /// The default action of activating a `<button>`: submit or reset its
    /// form. `false` when `node` isn't an enabled button with a form.
    pub(crate) fn activate_form_button(&mut self, node: NodeId, via_keyboard: bool) -> bool {
        let Some(kind) = button_kind(&self.arena, node) else {
            return false;
        };
        if self.arena.is_disabled(node) {
            return false;
        }
        let Some(form) = form::owner(&self.arena, node) else {
            return false;
        };
        match kind {
            ButtonKind::Submit => {
                self.submit_form(form, Some(node), via_keyboard);
                true
            }
            ButtonKind::Reset => {
                self.reset_form(form);
                true
            }
            ButtonKind::Plain => false,
        }
    }

    /// What Enter in text field `field` does, if anything.
    pub(crate) fn implicit_submission(&self, field: NodeId) -> Option<ImplicitSubmit> {
        let form = form::owner(&self.arena, field)?;
        match form::default_button(&self.arena, form) {
            Some(button) if self.arena.is_disabled(button) => None,
            Some(button) => Some(ImplicitSubmit::Click(button)),
            None => form::is_only_text_field(&self.arena, form, field)
                .then_some(ImplicitSubmit::Submit(form)),
        }
    }
}
