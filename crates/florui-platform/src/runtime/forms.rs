//! `<form>` submission, reset and pseudo-class state on [`UiRuntime`].

use florui_style::{FocusPath, FormState, NodeId};

use super::UiRuntime;
use crate::form;
use crate::form_control::{ButtonKind, FormContext, button_kind};

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
    fn with_form_context<R>(&self, f: impl FnOnce(&FormContext) -> R) -> R {
        let edited = self.edited.borrow();
        f(&FormContext {
            arena: &self.arena,
            options: &self.option_summaries,
            edited: &edited,
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
                return SubmitOutcome::Blocked;
            }
        }
        let data = self.with_form_context(|ctx| form::form_data(ctx, form, submitter));
        if let Some(handler) = self.arena.submit_handler(form).cloned() {
            florui_reactive::batch(|| handler.call(data));
        }
        SubmitOutcome::Submitted
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
