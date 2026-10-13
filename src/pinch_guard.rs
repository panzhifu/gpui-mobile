//! Turns a second live contact into a pinch, and stops the pan that second
//! finger would otherwise drive from fighting the zoom.
//!
//! GPUI 0.3.8 accepts `PlatformInput::Pinch` — `window.rs:5513` records its
//! position and modifiers, `window.rs:5590` hands it to `dispatch_mouse_event`,
//! and that bubbles it to every `on_mouse_event` listener because `PinchEvent`
//! implements `MouseEvent` — but no platform backend in that version ever
//! produces one, neither a desktop backend nor this crate's touch bridge. The
//! recognizer inside GPUI cannot synthesize one either: `TouchGestureState`
//! holds exactly one `ActiveTouch`, so a second contact arriving mid-gesture is
//! dropped while the first finger's movement keeps flowing out as scroll events.
//! A two-finger gesture offered to that recognizer pans and flings the page
//! instead of scaling it.
//!
//! So the pinch is recognized here, where every pointer of every `MotionEvent`
//! and every `UITouch` is still distinguishable by ID. The rule is the one
//! Android and iOS themselves apply: once a second finger is down, the gesture
//! is no longer a pan. The first contact is cancelled on the recognizer's behalf
//! — that ends any pan it had begun, and because a cancelled contact never
//! starts a fling, no momentum is armed to fight the zoom — and every contact of
//! the session is then swallowed rather than forwarded, so the finger left on
//! the screen after the pinch cannot resume panning until it lifts and is put
//! down again.
//!
//! The emitted `delta` is a relative change, matching what GPUI documents the
//! field to mean ("0.1 represents a 10% zoom increase"): each step is the ratio
//! between the current spacing of the two earliest contacts and the spacing at
//! the previous emitted step. A consumer multiplies its scale by `1.0 + delta`,
//! which makes the zoom independent of how fast the fingers moved and of where
//! the gesture began.
//!
//! One limitation is stated rather than papered over: a contact is only known to
//! be lifted once its event reaches this guard. Where a native platform view
//! claims a touch — either Android route returns before the window is consulted
//! (`android/host.rs:616`, `android/jni.rs:487`) — the guard keeps counting that
//! contact, and the session then goes on swallowing contacts until each has been
//! reported up. An app that hosts no native platform view cannot reach that
//! state.
//!
//! Main thread only, like everything in GPUI.

use gpui::{point, px, Modifiers, Pixels, PlatformInput, Point, TouchEvent, TouchId, TouchPhase};

use crate::fling_guard::FlingGuard;

/// The smallest relative spacing change reported as a zoom step.
///
/// Two held fingers vary their spacing by a fraction of a percent from frame to
/// frame; reporting that would make a still page creep. Movement below the slop
/// is not discarded — the spacing baseline only advances when a step is
/// emitted, so successive sub-slop changes accumulate until one crosses it.
const ZOOM_SLOP: f32 = 0.005;

/// Relays single-finger contacts to [`FlingGuard`] unchanged and turns two or
/// more into [`PlatformInput::Pinch`] steps.
pub(crate) struct PinchGuard {
    /// Every contact still on the screen, in the order they arrived, each with
    /// its last reported position. The two earliest are the pinch's reference
    /// pair: a third finger keeps the session alive but never moves the pair,
    /// because its own position says nothing about the zoom the user is asking
    /// for.
    contacts: Vec<(TouchId, Point<Pixels>)>,
    /// A pinch has begun and at least one of its contacts is still down. While
    /// this is set no contact reaches GPUI, because the recognizer would read
    /// the remaining finger as a fresh pan.
    session: bool,
    /// Two or more contacts are down, so pinch steps are being reported.
    pinching: bool,
    /// Spacing of the reference pair when the last step was emitted, or `None`
    /// when the next movement has to measure again before it can report a ratio
    /// — which is the case right after the pair changed because a finger joined
    /// or lifted.
    baseline: Option<f32>,
}

impl PinchGuard {
    pub(crate) fn new() -> Self {
        Self {
            contacts: Vec::new(),
            session: false,
            pinching: false,
            baseline: None,
        }
    }

    /// Forward one raw contact to GPUI.
    ///
    /// `fling` and `next_id` are the ones the platform's touch bridge already
    /// uses: a single-finger contact has to reach GPUI exactly as it does today,
    /// fling guard included, so this guard sits in front of that one rather than
    /// beside it.
    pub(crate) fn relay(
        &mut self,
        event: TouchEvent,
        fling: &mut FlingGuard,
        next_id: &mut dyn FnMut() -> TouchId,
        emit: &mut dyn FnMut(PlatformInput),
    ) {
        self.remember(&event);

        // A second contact is what makes a pinch. Until one arrives, the first
        // contact is an ordinary touch and must not be filtered.
        if !self.session && self.contacts.len() >= 2 {
            self.begin(&event, fling, next_id, emit);
            return;
        }

        if !self.session {
            fling.relay(event, next_id, |touch| emit(PlatformInput::Touch(touch)));
            return;
        }

        self.step(&event, emit);

        // Once every finger has lifted, the next contact is an ordinary touch
        // again and reaches GPUI's recognizer unfiltered.
        if self.contacts.is_empty() {
            self.session = false;
            self.pinching = false;
            self.baseline = None;
        }
    }

    /// Keep the contact table current. The platform reports the same ID for a
    /// contact's every phase and never reuses one while it is down.
    fn remember(&mut self, event: &TouchEvent) {
        match event.phase {
            TouchPhase::Started => {
                if !self.contacts.iter().any(|(id, _)| *id == event.id) {
                    self.contacts.push((event.id, event.position));
                }
            }
            TouchPhase::Moved => {
                if let Some((_, position)) =
                    self.contacts.iter_mut().find(|(id, _)| *id == event.id)
                {
                    *position = event.position;
                } else {
                    // A movement for a contact this guard never saw begin — the
                    // platform can drop a `Started` while a window is being
                    // attached — counts as one that has just begun, so the
                    // reference pair is not left working from a stale position.
                    self.contacts.push((event.id, event.position));
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if let Some(index) = self.contacts.iter().position(|(id, _)| *id == event.id) {
                    self.contacts.remove(index);
                }
            }
        }
    }

    /// Open the session: end the pan the first contact may already be driving,
    /// then report the pinch as begun without a zoom step.
    fn begin(
        &mut self,
        newcomer: &TouchEvent,
        fling: &mut FlingGuard,
        next_id: &mut dyn FnMut() -> TouchId,
        emit: &mut dyn FnMut(PlatformInput),
    ) {
        self.session = true;
        self.pinching = true;

        // Cancel the contact that is not the newcomer: that is the one GPUI's
        // recognizer believes it is tracking, and it is only ever one because
        // the session opens on the second contact. Its last reported position is
        // where the recognizer stops the pan.
        if let Some((id, position)) = self
            .contacts
            .iter()
            .find(|(id, _)| *id != newcomer.id)
            .copied()
        {
            fling.relay(
                TouchEvent {
                    id,
                    phase: TouchPhase::Cancelled,
                    position,
                    predicted_position: None,
                    force: None,
                },
                next_id,
                |touch| emit(PlatformInput::Touch(touch)),
            );
        }

        self.baseline = self.spacing();
        emit(self.pinch(
            0.0,
            self.pair_center(newcomer.position),
            TouchPhase::Started,
        ));
    }

    /// Report one pinch step, or close the pinch when the pair no longer exists.
    fn step(&mut self, event: &TouchEvent, emit: &mut dyn FnMut(PlatformInput)) {
        if self.contacts.len() >= 2 {
            match event.phase {
                TouchPhase::Moved => {
                    let Some(spacing) = self.spacing() else {
                        return;
                    };
                    let Some(baseline) = self.baseline.filter(|baseline| *baseline > 0.0) else {
                        // Measure first without reporting: the pair just changed,
                        // so the old spacing says nothing about this one.
                        self.baseline = Some(spacing);
                        return;
                    };
                    let delta = spacing / baseline - 1.0;
                    if delta.abs() >= ZOOM_SLOP {
                        emit(self.pinch(
                            delta,
                            self.pair_center(event.position),
                            TouchPhase::Moved,
                        ));
                        self.baseline = Some(spacing);
                    }
                }
                TouchPhase::Started => {
                    // A finger put down again while another is still on the
                    // screen from a pinch that has closed opens a second pinch,
                    // so the user can keep scaling without lifting everything.
                    if !self.pinching {
                        self.pinching = true;
                        self.baseline = self.spacing();
                        emit(self.pinch(
                            0.0,
                            self.pair_center(event.position),
                            TouchPhase::Started,
                        ));
                    }
                }
                _ => {}
            }
            return;
        }

        // Fewer than two contacts are left, so the gesture is over: the pinch
        // closes at wherever the lifting finger was. What remains of the session
        // is swallowed until every finger has gone.
        if self.pinching {
            self.pinching = false;
            self.baseline = None;
            let phase = if event.phase == TouchPhase::Cancelled {
                TouchPhase::Cancelled
            } else {
                TouchPhase::Ended
            };
            emit(self.pinch(0.0, event.position, phase));
        } else {
            self.baseline = None;
        }
    }

    /// Spacing of the two earliest contacts, in window coordinates.
    fn spacing(&self) -> Option<f32> {
        if self.contacts.len() < 2 {
            return None;
        }
        Some((self.contacts[1].1 - self.contacts[0].1).magnitude() as f32)
    }

    /// The point the zoom is centered on: the middle of the reference pair, and
    /// where GPUI places the mouse. `fallback` covers the instant a pair has
    /// just broken apart.
    fn pair_center(&self, fallback: Point<Pixels>) -> Point<Pixels> {
        if self.contacts.len() < 2 {
            return fallback;
        }
        let (a, b) = (self.contacts[0].1, self.contacts[1].1);
        point(
            px((f32::from(a.x) + f32::from(b.x)) / 2.0),
            px((f32::from(a.y) + f32::from(b.y)) / 2.0),
        )
    }

    fn pinch(&self, delta: f32, position: Point<Pixels>, phase: TouchPhase) -> PlatformInput {
        PlatformInput::Pinch(gpui::PinchEvent {
            position,
            delta,
            // A touch screen reports no keyboard modifiers.
            modifiers: Modifiers::default(),
            phase,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What left the guard, in order: a relayed contact as `Touch`, a zoom step
    /// as `Pinch`.
    #[derive(Debug, PartialEq)]
    enum Out {
        Touch(u64, TouchPhase),
        Pinch(TouchPhase, f32),
    }

    fn touch(id: u64, phase: TouchPhase, x: f32, y: f32) -> TouchEvent {
        TouchEvent {
            id: TouchId(id),
            phase,
            position: point(px(x), px(y)),
            predicted_position: None,
            force: None,
        }
    }

    /// Runs `events` through a guard and the fling guard behind it, keeping that
    /// pair alive across calls the way the platform bridge does.
    fn relay(
        guard: &mut PinchGuard,
        fling: &mut FlingGuard,
        events: impl IntoIterator<Item = TouchEvent>,
    ) -> Vec<Out> {
        let mut next_id = 100u64;
        let mut out = Vec::new();
        for event in events {
            guard.relay(
                event,
                fling,
                &mut || {
                    next_id += 1;
                    TouchId(next_id)
                },
                &mut |input| match input {
                    PlatformInput::Touch(touch) => out.push(Out::Touch(touch.id.0, touch.phase)),
                    PlatformInput::Pinch(pinch) => out.push(Out::Pinch(pinch.phase, pinch.delta)),
                    other => panic!("the guard forwarded {other:?}, which is not touch input"),
                },
            );
        }
        out
    }

    /// Zoom steps are ratios of two measured distances, so compare them with the
    /// room a float needs rather than bit for bit.
    fn assert_out(out: &[Out], expected: &[Out]) {
        let same = out.len() == expected.len()
            && out
                .iter()
                .zip(expected)
                .all(|(got, want)| match (got, want) {
                    (Out::Touch(a, pa), Out::Touch(b, pb)) => a == b && pa == pb,
                    (Out::Pinch(pa, da), Out::Pinch(pb, db)) => pa == pb && (da - db).abs() < 1e-6,
                    _ => false,
                });
        assert!(same, "got {out:?}, expected {expected:?}");
    }

    /// One ordinary pan: down, twice past the slop, up.
    fn pan(guard: &mut PinchGuard, fling: &mut FlingGuard) -> Vec<Out> {
        relay(
            guard,
            fling,
            [
                touch(1, TouchPhase::Started, 300., 300.),
                touch(1, TouchPhase::Moved, 250., 300.),
                touch(1, TouchPhase::Moved, 200., 300.),
                touch(1, TouchPhase::Ended, 150., 300.),
            ],
        )
    }

    #[test]
    fn a_single_finger_is_relayed_exactly_as_it_arrives() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();

        let out = pan(&mut guard, &mut fling);

        assert_out(
            &out,
            &[
                Out::Touch(1, TouchPhase::Started),
                Out::Touch(1, TouchPhase::Moved),
                Out::Touch(1, TouchPhase::Moved),
                Out::Touch(1, TouchPhase::Ended),
            ],
        );
    }

    #[test]
    fn a_second_contact_cancels_the_first_and_opens_the_pinch() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();

        let out = relay(
            &mut guard,
            &mut fling,
            [
                touch(1, TouchPhase::Started, 100., 300.),
                touch(1, TouchPhase::Moved, 100., 320.),
                touch(2, TouchPhase::Started, 300., 320.),
            ],
        );

        assert_out(
            &out,
            &[
                Out::Touch(1, TouchPhase::Started),
                Out::Touch(1, TouchPhase::Moved),
                // The pan the recognizer is driving has to end here...
                Out::Touch(1, TouchPhase::Cancelled),
                // ...and this contact never reaches it at all.
                Out::Pinch(TouchPhase::Started, 0.0),
            ],
        );
    }

    #[test]
    fn spreading_the_fingers_reports_the_relative_change() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();

        let out = relay(
            &mut guard,
            &mut fling,
            [
                touch(1, TouchPhase::Started, 100., 300.),
                touch(2, TouchPhase::Started, 300., 300.),
                // 200 px apart, then 300 px apart.
                touch(1, TouchPhase::Moved, 0., 300.),
            ],
        );
        assert_out(
            &out,
            &[
                Out::Touch(1, TouchPhase::Started),
                Out::Touch(1, TouchPhase::Cancelled),
                Out::Pinch(TouchPhase::Started, 0.0),
                Out::Pinch(TouchPhase::Moved, 0.5),
            ],
        );

        // Closing the fingers again reports a negative step against the spacing
        // of the step that was emitted, not against the original spacing.
        let out = relay(
            &mut guard,
            &mut fling,
            [touch(1, TouchPhase::Moved, 150., 300.)],
        );
        assert_out(&out, &[Out::Pinch(TouchPhase::Moved, -0.5)]);
    }

    #[test]
    fn the_step_is_measured_from_the_pair_center() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();
        relay(
            &mut guard,
            &mut fling,
            [
                touch(1, TouchPhase::Started, 100., 300.),
                touch(2, TouchPhase::Started, 300., 500.),
            ],
        );

        let mut center = None;
        guard.relay(
            touch(1, TouchPhase::Moved, 0., 100.),
            &mut fling,
            &mut || TouchId(9),
            &mut |input| {
                if let PlatformInput::Pinch(pinch) = input {
                    center = Some(pinch.position);
                }
            },
        );

        // The pair is now (0, 100) and (300, 500).
        assert_eq!(center, Some(point(px(150.), px(300.))));
    }

    #[test]
    fn a_still_pinch_reports_nothing_but_does_not_lose_the_drift() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();
        relay(
            &mut guard,
            &mut fling,
            [
                touch(1, TouchPhase::Started, 100., 300.),
                touch(2, TouchPhase::Started, 300., 300.),
            ],
        );

        // Four steps that each change the spacing by a fraction of the slop:
        // none of them crosses it alone, so the one that is reported has to
        // carry all four.
        let mut out = Vec::new();
        for shift in [0.25, 0.5, 0.75, 1.5] {
            out.extend(relay(
                &mut guard,
                &mut fling,
                [touch(1, TouchPhase::Moved, 100. - shift, 300.)],
            ));
        }

        assert_out(&out, &[Out::Pinch(TouchPhase::Moved, 1.5 / 200.0)]);
    }

    #[test]
    fn lifting_one_finger_ends_the_pinch_and_the_other_cannot_resume_a_pan() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();

        let out = relay(
            &mut guard,
            &mut fling,
            [
                touch(1, TouchPhase::Started, 100., 300.),
                touch(2, TouchPhase::Started, 200., 300.),
                touch(2, TouchPhase::Ended, 200., 300.),
                touch(1, TouchPhase::Moved, 400., 300.),
                touch(1, TouchPhase::Ended, 400., 300.),
            ],
        );

        assert_out(
            &out,
            &[
                Out::Touch(1, TouchPhase::Started),
                Out::Touch(1, TouchPhase::Cancelled),
                Out::Pinch(TouchPhase::Started, 0.0),
                Out::Pinch(TouchPhase::Ended, 0.0),
            ],
        );
    }

    #[test]
    fn a_third_finger_does_not_move_the_reference_pair() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();

        let out = relay(
            &mut guard,
            &mut fling,
            [
                touch(1, TouchPhase::Started, 100., 300.),
                touch(2, TouchPhase::Started, 300., 300.),
                touch(3, TouchPhase::Started, 500., 500.),
                // The two earliest contacts are still 200 px apart, so the
                // third finger's own travel is not a zoom.
                touch(3, TouchPhase::Moved, 900., 500.),
            ],
        );

        assert_out(
            &out,
            &[
                Out::Touch(1, TouchPhase::Started),
                Out::Touch(1, TouchPhase::Cancelled),
                Out::Pinch(TouchPhase::Started, 0.0),
            ],
        );
    }

    #[test]
    fn a_pinch_reopened_over_a_remaining_finger_is_reported() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();
        relay(
            &mut guard,
            &mut fling,
            [
                touch(1, TouchPhase::Started, 100., 300.),
                touch(2, TouchPhase::Started, 200., 300.),
                touch(2, TouchPhase::Ended, 200., 300.),
            ],
        );

        let out = relay(
            &mut guard,
            &mut fling,
            [
                // The pair is (1, 3) now: 300 px apart, then 150 px.
                touch(3, TouchPhase::Started, 400., 300.),
                touch(1, TouchPhase::Moved, 250., 300.),
            ],
        );

        assert_out(
            &out,
            &[
                Out::Pinch(TouchPhase::Started, 0.0),
                Out::Pinch(TouchPhase::Moved, -0.5),
            ],
        );
    }

    #[test]
    fn the_next_gesture_after_every_finger_has_lifted_is_an_ordinary_pan() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();
        relay(
            &mut guard,
            &mut fling,
            [
                touch(1, TouchPhase::Started, 100., 300.),
                touch(2, TouchPhase::Started, 200., 300.),
                touch(1, TouchPhase::Ended, 100., 300.),
                touch(2, TouchPhase::Ended, 200., 300.),
            ],
        );

        let out = pan(&mut guard, &mut fling);

        assert_out(
            &out,
            &[
                Out::Touch(1, TouchPhase::Started),
                Out::Touch(1, TouchPhase::Moved),
                Out::Touch(1, TouchPhase::Moved),
                Out::Touch(1, TouchPhase::Ended),
            ],
        );
    }

    #[test]
    fn a_moved_contact_never_seen_beginning_is_counted_as_new() {
        let mut guard = PinchGuard::new();
        let mut fling = FlingGuard::new();

        // A `Started` dropped while the window was being attached must not make
        // the following contact invisible to the pinch: two contacts are down
        // whether or not the guard saw the first one arrive.
        let out = relay(
            &mut guard,
            &mut fling,
            [
                touch(1, TouchPhase::Moved, 100., 300.),
                touch(2, TouchPhase::Started, 300., 300.),
            ],
        );

        assert_out(
            &out,
            &[
                Out::Touch(1, TouchPhase::Moved),
                Out::Touch(1, TouchPhase::Cancelled),
                Out::Pinch(TouchPhase::Started, 0.0),
            ],
        );
    }

    /// The iOS bridge holds both guards in a `RefCell` and calls through a
    /// borrowed pair. That shape is not compiled for any target this repository
    /// can build on a Linux host, so it is exercised here instead: a guard kept
    /// behind interior mutability must behave exactly like the direct pair the
    /// Android bridge uses.
    #[test]
    fn the_guards_work_behind_the_interior_mutability_the_ios_bridge_uses() {
        use std::cell::{Cell, RefCell};

        struct Bridge {
            pinch: RefCell<PinchGuard>,
            fling: RefCell<FlingGuard>,
            next_id: Cell<u64>,
            out: RefCell<Vec<Out>>,
        }

        let bridge = Bridge {
            pinch: RefCell::new(PinchGuard::new()),
            fling: RefCell::new(FlingGuard::new()),
            next_id: Cell::new(0),
            out: RefCell::new(Vec::new()),
        };

        for event in [
            touch(1, TouchPhase::Started, 100., 300.),
            touch(2, TouchPhase::Started, 300., 300.),
            touch(1, TouchPhase::Moved, 0., 300.),
        ] {
            let mut pinch = bridge.pinch.borrow_mut();
            let mut fling = bridge.fling.borrow_mut();
            let mut next_id = || {
                let id = TouchId(bridge.next_id.get());
                bridge.next_id.set(id.0 + 1);
                id
            };
            let mut emit = |input: PlatformInput| match input {
                PlatformInput::Touch(touch) => bridge
                    .out
                    .borrow_mut()
                    .push(Out::Touch(touch.id.0, touch.phase)),
                PlatformInput::Pinch(pinch) => bridge
                    .out
                    .borrow_mut()
                    .push(Out::Pinch(pinch.phase, pinch.delta)),
                _ => {}
            };
            pinch.relay(event, &mut fling, &mut next_id, &mut emit);
        }

        assert_out(
            bridge.out.borrow().as_slice(),
            &[
                Out::Touch(1, TouchPhase::Started),
                Out::Touch(1, TouchPhase::Cancelled),
                Out::Pinch(TouchPhase::Started, 0.0),
                Out::Pinch(TouchPhase::Moved, 0.5),
            ],
        );
    }
}
