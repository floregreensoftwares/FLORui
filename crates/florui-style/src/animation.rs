//! Persistent state real `transition`/`@keyframes` animation needs across
//! [`crate::cascade::compute`] calls, which is otherwise a pure function of
//! its arguments (see [`crate::stylo`]'s own module doc): Stylo's real
//! animation engine (`style::animation`) samples an in-progress transition
//! or animation against the *previous* cascade's computed value and an
//! [`style::animation::ElementAnimationSet`] it expects to find again next
//! frame — both of which need somewhere to live between one `compute()`
//! call and the next.
//!
//! [`Arena`](crate::tree::Arena) rebuilds from scratch every call with no
//! stable per-element identity (a `NodeId` is just preorder position), so
//! this also owns the identity scheme animation state is actually keyed
//! by: each element's `tag` and position among same-tag siblings, chained
//! up to the document root. Deliberately *not* class-sensitive — a class
//! toggle or a `:hover` match changing is the most common way real CSS
//! ever triggers a transition in the first place, so identity has to
//! survive exactly that kind of change to be useful. Two elements in one
//! tree can never collide (their paths diverge at the first differing
//! ordinal), and the common case — a render that doesn't reorder or
//! add/remove same-tag siblings around this one — keeps the same identity
//! call over call, so its animation continues rather than restarting. A
//! same-tag element swapped in at the same position (a conditional
//! branch, a keyless list reorder) can still be mistaken for the outgoing
//! one and inherit its in-flight animation — the same honest ambiguity
//! any keyless positional matching has, real CSS included.

use std::collections::HashMap;

use style::animation::{AnimationState, DocumentAnimationSet, ElementAnimationSet};
use style::properties::ComputedValues;
use style::servo_arc::Arc as StyloArc;

/// One element's identity for animation purposes — see the module doc.
#[derive(Clone, PartialEq, Eq, Hash)]
struct PathKey {
    parent: Option<usize>,
    tag: &'static str,
    ordinal: usize,
}

/// Carries real Stylo animation state across [`crate::cascade::compute`]
/// calls. Cheap to construct; a real desktop session keeps one alive for
/// its whole lifetime, reusing it every frame so in-progress animations
/// keep sampling from where they left off. Tests that want a specific,
/// reproducible instant construct one and call [`Self::advance_to`]
/// directly rather than reading a real clock — the "shared controllable
/// clock" this crate's own motion spec calls for.
///
/// Also the carrier for host-supplied display-preference signals
/// (`prefers-reduced-motion`, `prefers-color-scheme`) that need to reach
/// [`crate::stylo::device`] — not because either is an animation-timing
/// concept, but because this struct is already threaded through every
/// `compute()` call site that needs them, and adding a second public
/// parameter everywhere `&mut AnimationTimeline` already flows would
/// break two public signatures (`crate::cascade::compute`/
/// `compute_with_container_query_signature`, and
/// `florui_layout::compute_with_style`) for no functional gain.
/// Tells one timeline from another, for what is kept only while the same one
/// is used (the identities it handed out mean nothing to any other).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TimelineId(u64);

impl Default for TimelineId {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        TimelineId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

#[derive(Default)]
pub struct AnimationTimeline {
    id: TimelineId,
    pub(crate) now: f64,
    interner: HashMap<PathKey, usize>,
    next_id: usize,
    pub(crate) sets: DocumentAnimationSet,
    previous_styles: HashMap<usize, StyloArc<ComputedValues>>,
    touched_this_call: Vec<usize>,
    /// The real OS accessibility preference, as of the last time a host
    /// pushed it in — `false` (the `Default` value) for every caller that
    /// never does, which is every caller except a real desktop host (tests,
    /// benches, `florui-conformance`'s deterministic snapshots). Read-only
    /// truth for `@media (prefers-reduced-motion: ...)`; see
    /// [`Self::should_suppress_animations`] for the separate, opt-out-able
    /// mechanism this alone does not drive.
    os_prefers_reduced_motion: bool,
    /// Inverted so the derived `Default` (`false`) means "not disabled",
    /// i.e. auto-suppression enabled — the required default — without
    /// `new()` needing to diverge from `default()` (every non-desktop
    /// caller, including `Default::default()` call sites this crate can't
    /// see, must agree). Read via [`Self::should_suppress_animations`];
    /// written via [`Self::set_auto_suppress_motion`], which keeps the
    /// public API framed positively (`true` = suppress).
    auto_suppress_motion_disabled: bool,
    /// The real OS/window color-scheme preference, as of the last time a
    /// host pushed it in — `false` (light, the `Default` value) for every
    /// caller that never does. Unlike reduced motion, this is already the
    /// *effective* value (an explicit `WindowOptions.theme` override, if
    /// any, is resolved before it ever reaches here) — `florui-platform`
    /// has no way to separately recover "what the OS truly prefers
    /// regardless of an active override" once one is set (a real `winit`
    /// limitation, not a choice this crate made), so there is only ever
    /// one signal to carry, not two.
    prefers_dark_color_scheme: bool,
}

/// One kind of running animation, with how many elements run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningAnimation {
    pub kind: RunningKind,
    pub elements: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunningKind {
    /// An `animation-name`, repeating for ever when `infinite`.
    Keyframes { name: String, infinite: bool },
    /// A transition of `property`.
    Transition { property: String },
}

impl AnimationTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// The real OS accessibility preference — used only for
    /// `@media (prefers-reduced-motion: ...)`, independent of
    /// [`Self::should_suppress_animations`]'s own opt-out: an author who
    /// explicitly wrote that media query deserves the real answer
    /// regardless of whether this host opted out of automatic suppression.
    pub(crate) fn prefers_reduced_motion(&self) -> bool {
        self.os_prefers_reduced_motion
    }

    /// Pushes a freshly-read OS accessibility preference in — a real host
    /// calls this once per relevant update, since nothing here reads the
    /// OS itself (this crate has no OS integration at all; see
    /// `florui_platform::accessibility`).
    pub fn set_os_prefers_reduced_motion(&mut self, value: bool) {
        self.os_prefers_reduced_motion = value;
    }

    /// Opts into (`true`, the default) or out of (`false`) automatic
    /// transition/`@keyframes` suppression when the OS prefers reduced
    /// motion. Does not affect `@media (prefers-reduced-motion: ...)`
    /// itself, which always reflects the real OS truth.
    pub fn set_auto_suppress_motion(&mut self, value: bool) {
        self.auto_suppress_motion_disabled = !value;
    }

    /// Whether `transition`/`@keyframes` animations should be suppressed
    /// outright this call — both the OS preference and the opt-out must
    /// agree.
    pub(crate) fn should_suppress_animations(&self) -> bool {
        !self.auto_suppress_motion_disabled && self.os_prefers_reduced_motion
    }

    /// The effective color-scheme preference — see the field's own doc for
    /// why this is already resolved (override-or-OS), not a separate pair
    /// of signals the way reduced motion's are.
    pub(crate) fn prefers_dark_color_scheme(&self) -> bool {
        self.prefers_dark_color_scheme
    }

    /// Pushes a freshly-resolved effective color scheme in — a real host
    /// calls this once at window construction and again on every live
    /// `WindowEvent::ThemeChanged` (only while no explicit
    /// `WindowOptions.theme` override is active; an override already makes
    /// the window immune to OS changes on the `florui-platform` side).
    pub fn set_prefers_dark_color_scheme(&mut self, value: bool) {
        self.prefers_dark_color_scheme = value;
    }

    /// Sets the instant `compute()` samples any in-progress animation or
    /// transition against. Seconds, on any epoch this timeline is
    /// internally consistent about — real callers reuse one
    /// [`std::time::Instant`] captured once at startup; nothing here reads
    /// a wall clock itself.
    pub fn advance_to(&mut self, seconds: f64) {
        self.now = seconds;
    }

    /// Whether anything this timeline is tracking still needs another
    /// frame to keep progressing — the caller's cue to keep scheduling
    /// redraws instead of going idle. `false` whenever
    /// [`Self::should_suppress_animations`] is active: Stylo's own
    /// bookkeeping (`sets`) still tracks a suppressed animation internally
    /// (see [`crate::stylo`]'s own suppression point, which leaves this
    /// untouched on purpose), but nothing suppressed is visibly changing
    /// frame to frame, so scheduling more redraws for it would be pointless.
    pub fn is_animating(&self) -> bool {
        if self.should_suppress_animations() {
            return false;
        }
        self.sets
            .sets
            .read()
            .values()
            .any(ElementAnimationSet::needs_animation_ticks)
    }

    /// What is running or waiting for a tick, counted by what it is and not by
    /// element: how many elements run each `@keyframes` (and whether it repeats
    /// for ever) and change each transitioning property. What keeps a window
    /// asking for frames, named, for a report. Empty when motion is suppressed,
    /// as [`Self::is_animating`] is.
    pub fn running(&self) -> Vec<RunningAnimation> {
        if self.should_suppress_animations() {
            return Vec::new();
        }
        let ticking = |state: &AnimationState| {
            matches!(state, AnimationState::Running | AnimationState::Pending)
        };
        let mut counts: std::collections::BTreeMap<(bool, String, bool), usize> =
            std::collections::BTreeMap::new();
        for set in self.sets.sets.read().values() {
            for animation in set.animations.iter().filter(|a| ticking(&a.state)) {
                let infinite = matches!(
                    animation.iteration_state,
                    style::animation::KeyframesIterationState::Infinite(_)
                );
                *counts
                    .entry((true, animation.name.to_string(), infinite))
                    .or_default() += 1;
            }
            for transition in set.transitions.iter().filter(|t| ticking(&t.state)) {
                let property = transition.property_animation.property_id().name();
                *counts
                    .entry((false, property.into_owned(), false))
                    .or_default() += 1;
            }
        }
        counts
            .into_iter()
            .map(|((keyframes, name, infinite), elements)| RunningAnimation {
                kind: if keyframes {
                    RunningKind::Keyframes { name, infinite }
                } else {
                    RunningKind::Transition { property: name }
                },
                elements,
            })
            .collect()
    }

    /// Whether no animation or transition is running or waiting for a tick, so
    /// the styles of a tree that did not change cannot change with time.
    /// Unlike [`Self::is_animating`] this does not look away from an
    /// animation that motion settings suppress.
    pub fn is_still(&self) -> bool {
        !self
            .sets
            .sets
            .read()
            .values()
            .any(ElementAnimationSet::needs_animation_ticks)
    }

    /// The host settings that change how the cascade resolves a tree: the
    /// operating system's reduced-motion preference, whether it is acted on,
    /// and the color scheme.
    pub fn settings(&self) -> (bool, bool, bool) {
        (
            self.os_prefers_reduced_motion,
            self.auto_suppress_motion_disabled,
            self.prefers_dark_color_scheme,
        )
    }

    /// Looks up (or assigns) `id`'s stable identity, given its already-
    /// resolved parent identity (`None` for a document root). Must be
    /// called in preorder — a node's own identity has to exist before any
    /// of its children can reference it.
    pub(crate) fn stable_id(
        &mut self,
        parent: Option<usize>,
        tag: &'static str,
        ordinal: usize,
    ) -> usize {
        let key = PathKey {
            parent,
            tag,
            ordinal,
        };
        let id = *self.interner.entry(key).or_insert_with(|| {
            let id = self.next_id;
            self.next_id += 1;
            id
        });
        self.touched_this_call.push(id);
        id
    }

    pub(crate) fn id(&self) -> TimelineId {
        self.id
    }

    /// Counts identities as met by this call without looking each one up, for a
    /// caller that kept the tree they were handed out for.
    pub(crate) fn touch(&mut self, ids: impl IntoIterator<Item = usize>) {
        self.touched_this_call.extend(ids);
    }

    pub(crate) fn previous_style(&self, id: usize) -> Option<StyloArc<ComputedValues>> {
        self.previous_styles.get(&id).cloned()
    }

    pub(crate) fn set_current_style(&mut self, id: usize, style: StyloArc<ComputedValues>) {
        self.previous_styles.insert(id, style);
    }

    /// Drops tracked state for every identity `compute()` didn't touch
    /// this call — real CSS stops a transition/animation outright when its
    /// element leaves the document, not something worth keeping around on
    /// the chance it reappears.
    pub(crate) fn sweep(&mut self) {
        let mut touched = vec![false; self.next_id];
        for id in self.touched_this_call.drain(..) {
            touched[id] = true;
        }
        let met = |id: &usize| touched.get(*id).copied().unwrap_or(false);
        self.previous_styles.retain(|id, _| met(id));
        self.sets.sets.write().retain(|key, _| met(&key.node.0));
    }
}
