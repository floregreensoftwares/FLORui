//! Turns the per-file events `winit` reports on X11 into the whole-payload
//! sequence the rest of this crate delivers: one `Enter` for everything
//! hovering, then a `Leave`, or a `Drop` of everything dropped. `winit` has no
//! hover or drop events on Wayland and reports no pointer position during a
//! drag, so the position is the last one the pointer had over the window.
//!
//! Events are collected as they arrive and turned into drag events once per
//! loop iteration by [`DragTracker::flush`], which is what lets several files
//! that arrive together become one payload.

use std::path::PathBuf;

use crate::drag_drop::{DragEvent, DragPayload, DragPosition};

#[derive(Debug, Default)]
pub(crate) struct DragTracker {
    steps: Vec<Step>,
    /// `Some(accepted)` between an `Enter` and the `Leave` or `Drop` that ends it.
    entered: Option<bool>,
    position: Option<DragPosition>,
}

#[derive(Debug)]
enum Step {
    Hovered(PathBuf),
    Cancelled,
    Dropped(PathBuf),
}

impl DragTracker {
    pub(crate) fn hovered(&mut self, path: PathBuf) {
        self.steps.push(Step::Hovered(path));
    }

    pub(crate) fn cancelled(&mut self) {
        self.steps.push(Step::Cancelled);
    }

    pub(crate) fn dropped(&mut self, path: PathBuf) {
        self.steps.push(Step::Dropped(path));
    }

    pub(crate) fn pointer_at(&mut self, position: DragPosition) {
        self.position = Some(position);
    }

    /// The drag events for everything collected since the last call. `accept`
    /// is asked once per `Enter`; a `Drop` follows the answer given there, and
    /// a payload dropped without a hover is asked about when it lands. A
    /// rejected payload is never dropped, only left.
    pub(crate) fn flush(&mut self, accept: impl Fn(&DragPayload) -> bool) -> Vec<DragEvent> {
        let position = self.position.unwrap_or(DragPosition { x: 0.0, y: 0.0 });
        let mut events = Vec::new();
        let mut rest = std::mem::take(&mut self.steps).into_iter().peekable();
        while let Some(step) = rest.next() {
            match step {
                Step::Hovered(first) => {
                    let mut files = vec![first];
                    while let Some(Step::Hovered(path)) =
                        rest.next_if(|next| matches!(next, Step::Hovered(_)))
                    {
                        files.push(path);
                    }
                    if self.entered.take().is_some() {
                        events.push(DragEvent::Leave);
                    }
                    let payload = DragPayload::Files(files);
                    let accepted = accept(&payload);
                    self.entered = Some(accepted);
                    events.push(DragEvent::Enter {
                        payload,
                        position,
                        accepted,
                    });
                }
                Step::Cancelled => {
                    if self.entered.take().is_some() {
                        events.push(DragEvent::Leave);
                    }
                }
                Step::Dropped(first) => {
                    let mut files = vec![first];
                    while let Some(Step::Dropped(path)) =
                        rest.next_if(|next| matches!(next, Step::Dropped(_)))
                    {
                        files.push(path);
                    }
                    let payload = DragPayload::Files(files);
                    let hovered = self.entered.take();
                    let accepted = hovered.unwrap_or_else(|| accept(&payload));
                    if accepted {
                        events.push(DragEvent::Drop { payload, position });
                    } else if hovered.is_some() {
                        events.push(DragEvent::Leave);
                    }
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32) -> DragPosition {
        DragPosition { x, y }
    }

    fn files(names: &[&str]) -> DragPayload {
        DragPayload::Files(names.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn files_hovering_together_become_one_enter() {
        let mut tracker = DragTracker::default();
        tracker.pointer_at(at(40.0, 60.0));
        tracker.hovered("/a.txt".into());
        tracker.hovered("/b.txt".into());
        assert_eq!(
            tracker.flush(|_| true),
            vec![DragEvent::Enter {
                payload: files(&["/a.txt", "/b.txt"]),
                position: at(40.0, 60.0),
                accepted: true,
            }]
        );
    }

    #[test]
    fn cancelling_a_hover_leaves_once() {
        let mut tracker = DragTracker::default();
        tracker.hovered("/a.txt".into());
        tracker.flush(|_| true);
        tracker.cancelled();
        assert_eq!(tracker.flush(|_| true), vec![DragEvent::Leave]);
        tracker.cancelled();
        assert_eq!(tracker.flush(|_| true), vec![], "no second Leave");
    }

    #[test]
    fn a_hover_cancelled_in_the_same_batch_is_still_entered_and_left_in_order() {
        let mut tracker = DragTracker::default();
        tracker.hovered("/a.txt".into());
        tracker.cancelled();
        let events = tracker.flush(|_| true);
        assert!(matches!(
            events.as_slice(),
            [DragEvent::Enter { .. }, DragEvent::Leave]
        ));
    }

    #[test]
    fn a_drop_after_an_accepted_hover_delivers_the_files_dropped() {
        let mut tracker = DragTracker::default();
        tracker.pointer_at(at(5.0, 6.0));
        tracker.hovered("/a.txt".into());
        tracker.flush(|_| true);
        tracker.dropped("/a.txt".into());
        tracker.dropped("/b.txt".into());
        assert_eq!(
            tracker.flush(|_| panic!("the answer given at the hover is reused")),
            vec![DragEvent::Drop {
                payload: files(&["/a.txt", "/b.txt"]),
                position: at(5.0, 6.0),
            }]
        );
    }

    #[test]
    fn a_rejected_payload_is_left_and_never_dropped() {
        let mut tracker = DragTracker::default();
        tracker.hovered("/a.txt".into());
        let enter = tracker.flush(|_| false);
        assert!(matches!(
            enter[0],
            DragEvent::Enter {
                accepted: false,
                ..
            }
        ));
        tracker.dropped("/a.txt".into());
        assert_eq!(tracker.flush(|_| true), vec![DragEvent::Leave]);
    }

    #[test]
    fn a_drop_with_no_hover_is_judged_when_it_lands() {
        let mut tracker = DragTracker::default();
        tracker.dropped("/a.txt".into());
        let events =
            tracker.flush(|payload| matches!(payload, DragPayload::Files(f) if f.len() == 1));
        assert!(matches!(events.as_slice(), [DragEvent::Drop { .. }]));
        tracker.dropped("/a.txt".into());
        assert_eq!(
            tracker.flush(|_| false),
            vec![],
            "rejected, and there was no Enter to close"
        );
    }

    #[test]
    fn a_hover_inside_a_hover_closes_the_first() {
        let mut tracker = DragTracker::default();
        tracker.hovered("/a.txt".into());
        tracker.flush(|_| true);
        tracker.hovered("/b.txt".into());
        let events = tracker.flush(|_| true);
        assert!(matches!(events[0], DragEvent::Leave));
        assert!(
            matches!(&events[1], DragEvent::Enter { payload, .. } if *payload == files(&["/b.txt"]))
        );
    }

    #[test]
    fn without_a_known_pointer_the_position_is_the_origin() {
        let mut tracker = DragTracker::default();
        tracker.hovered("/a.txt".into());
        assert!(matches!(
            tracker.flush(|_| true)[0],
            DragEvent::Enter {
                position: DragPosition { x: 0.0, y: 0.0 },
                ..
            }
        ));
    }
}
