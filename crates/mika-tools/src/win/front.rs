//! Bringing a window to the front: the order of the ways and the check that
//! one of them worked.
//!
//! Windows lets a program change the foreground window only in a few cases
//! (it is the foreground program itself, it got the last input event, and so
//! on). A command said by voice starts outside all of them: a plain
//! `SetForegroundWindow` then returns FALSE, or returns TRUE and only flashes
//! the taskbar button. So every way is followed by a look at which window is
//! really in front, and the next way is tried when it is not the wanted one.
//!
//! Nothing here calls Windows: `wnd.rs` gives the real desktop, tests give a
//! scripted one.

/// One way to bring a window to the front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rung {
    /// `SetForegroundWindow` alone: enough when Windows already allows it.
    Plain,
    /// The same with the input of this thread attached to the thread of the
    /// window in front and to the thread of the wanted window.
    Attached,
    /// The same right after an input event sent by this process.
    AfterInput,
    /// `SwitchToThisWindow`, the way Alt+Tab switches.
    Switch,
}

/// The ways in the order they are tried: from the one with no side effects.
pub(super) const LADDER: [Rung; 4] = [Rung::Plain, Rung::Attached, Rung::AfterInput, Rung::Switch];

/// The pause between two looks at the foreground window.
pub(super) const FRONT_POLL: std::time::Duration = std::time::Duration::from_millis(35);

/// What Windows said to one try. Whether the window came forward is a
/// separate question: it is asked after every try that went out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tried {
    /// The call went out. The window may come forward a moment later.
    Sent,
    /// Windows refused the call at once.
    Refused,
    /// The way does not apply now: a hung window, keys held by the user.
    Skipped,
}

/// Why a window did not come forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrontError {
    /// The window is gone.
    Closed,
    /// Every way was tried and another window is still in front.
    Refused,
}

impl FrontError {
    /// The text for the model. It names no window: titles stay out of errors
    /// that go to the journal.
    pub(super) fn text(self) -> &'static str {
        match self {
            FrontError::Closed => "окно закрыто",
            // The window in front counts as much as the wanted one: input
            // can be neither attached to it nor sent while it is in front.
            FrontError::Refused => {
                "Windows не дал вывести окно вперёд. Скорее всего, это окно или окно, которое \
сейчас впереди, запущено от имени администратора: обычная программа такими окнами не управляет. \
Ещё так бывает, пока открыто меню «Пуск» или меню другой программы. Пользователь может \
переключиться на окно сам"
            }
        }
    }
}

/// Looks at the foreground window after one try: this many times at most.
/// Activation is asynchronous, the window comes forward a moment after the call.
pub(super) const CHECKS: u32 = 10;

/// What the ladder needs from the desktop.
pub(super) trait Desk {
    /// The wanted window still exists.
    fn exists(&mut self) -> bool;
    /// Takes the window out of the taskbar when it is minimized.
    fn restore(&mut self);
    /// The wanted window, or a dialog it owns, is the foreground window now.
    fn in_front(&mut self) -> bool;
    /// An input event may be sent now: the user holds no modifier key and
    /// the window in front, which gets the event, is not of the blind zone.
    fn input_allowed(&mut self) -> bool;
    fn attempt(&mut self, rung: Rung) -> Tried;
    /// A short wait between two looks at the foreground window.
    fn pause(&mut self);
}

/// Brings the window to the front. `Ok(None)`: it was in front already;
/// `Ok(Some(rung))`: this way worked. A call that Windows accepted is not
/// success by itself: only the window in front counts.
pub(super) fn bring_forward(desk: &mut dyn Desk) -> Result<Option<Rung>, FrontError> {
    if !desk.exists() {
        return Err(FrontError::Closed);
    }
    desk.restore();
    if desk.in_front() {
        return Ok(None);
    }
    for rung in LADDER {
        if !desk.exists() {
            return Err(FrontError::Closed);
        }
        if rung == Rung::AfterInput && !desk.input_allowed() {
            continue;
        }
        let checks = match desk.attempt(rung) {
            Tried::Skipped => continue,
            // Refused at once: one look is enough, nothing is on its way.
            Tried::Refused => 1,
            Tried::Sent => CHECKS,
        };
        for check in 0..checks {
            if check > 0 {
                desk.pause();
            }
            if desk.in_front() {
                return Ok(Some(rung));
            }
        }
    }
    Err(if desk.exists() {
        FrontError::Refused
    } else {
        FrontError::Closed
    })
}

/// The thread of a window, as the attached way sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Peer {
    pub thread: u32,
    /// The window answers messages now.
    pub answers: bool,
}

/// What the attached way does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AttachPlan {
    /// Threads whose input is joined to this thread.
    pub threads: Vec<u32>,
    /// The wanted window is also raised above the others.
    pub raise: bool,
}

/// Decides whose input the attached way joins to the thread `me`; `None`
/// when the way does not apply.
///
/// With joined input the activation is sent to the joined thread and waited
/// for. So a window that does not answer is never joined: the call would
/// wait for it for good, with this thread still attached to the window the
/// user works in. Such a wanted window is not raised either (raising asks its
/// thread too); it is only named to Windows, which then activates it on its
/// own, without waiting.
pub(super) fn attach_plan(me: u32, front: Option<Peer>, target: Peer) -> Option<AttachPlan> {
    let mut threads = Vec::new();
    for peer in front.into_iter().chain([target]) {
        let joins = peer.answers && peer.thread != 0 && peer.thread != me;
        if joins && !threads.contains(&peer.thread) {
            threads.push(peer.thread);
        }
    }
    (!threads.is_empty()).then_some(AttachPlan {
        threads,
        raise: target.answers,
    })
}

/// What is known about the window in front after a try.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Sight {
    /// It is the wanted window or a dialog that window owns.
    pub own: bool,
    /// It belongs to the thread of the wanted window.
    pub same_thread: bool,
    /// It was not in front before the first try.
    pub came: bool,
    /// The wanted window is disabled: a modal dialog of its program holds it.
    pub blocked: bool,
}

/// Whether the user now has the program she asked for. Besides the window
/// itself and its dialogs, that is a window the program put forward in its
/// place: some programs answer the activation of a main window with a modal
/// dialog that the window does not own. Another window of the same program
/// that was in front all along does not count (two windows of one browser).
pub(super) fn is_in_front(sight: Sight) -> bool {
    sight.own || (sight.same_thread && (sight.came || sight.blocked))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A desktop that follows a script.
    struct Script {
        alive: bool,
        /// The window is in front before anything is tried.
        in_front_already: bool,
        /// The way that works, and how many looks pass before the window is
        /// seen in front.
        works: Option<(Rung, u32)>,
        /// What Windows says to each way; `Sent` when not listed.
        answers: Vec<(Rung, Tried)>,
        /// The window closes when this way is tried.
        closes_at: Option<Rung>,
        /// The user holds a key, or a window of the blind zone is in front.
        no_input: bool,
        tried: Vec<Rung>,
        restored: u32,
        pauses: u32,
        /// Looks left until the window is in front; `None`: it is not coming.
        coming_in: Option<u32>,
    }

    impl Script {
        fn new() -> Self {
            Script {
                alive: true,
                in_front_already: false,
                works: None,
                answers: Vec::new(),
                closes_at: None,
                no_input: false,
                tried: Vec::new(),
                restored: 0,
                pauses: 0,
                coming_in: None,
            }
        }
    }

    impl Desk for Script {
        fn exists(&mut self) -> bool {
            self.alive
        }
        fn restore(&mut self) {
            self.restored += 1;
        }
        fn in_front(&mut self) -> bool {
            if self.in_front_already {
                return true;
            }
            match self.coming_in {
                Some(0) => true,
                Some(left) => {
                    self.coming_in = Some(left - 1);
                    false
                }
                None => false,
            }
        }
        fn input_allowed(&mut self) -> bool {
            !self.no_input
        }
        fn attempt(&mut self, rung: Rung) -> Tried {
            self.tried.push(rung);
            if self.closes_at == Some(rung) {
                self.alive = false;
            }
            if let Some((works, after)) = self.works {
                if works == rung {
                    self.coming_in = Some(after);
                }
            }
            self.answers
                .iter()
                .find(|(r, _)| *r == rung)
                .map(|(_, said)| *said)
                .unwrap_or(Tried::Sent)
        }
        fn pause(&mut self) {
            self.pauses += 1;
        }
    }

    #[test]
    fn the_first_way_is_enough_when_windows_allows_it() {
        let mut desk = Script::new();
        desk.works = Some((Rung::Plain, 0));
        assert_eq!(bring_forward(&mut desk), Ok(Some(Rung::Plain)));
        assert_eq!(desk.tried, vec![Rung::Plain]);
        assert_eq!(desk.restored, 1, "a minimized window is restored first");
        assert_eq!(desk.pauses, 0, "nothing to wait for");
    }

    #[test]
    fn a_window_already_in_front_is_left_alone() {
        let mut desk = Script::new();
        desk.in_front_already = true;
        assert_eq!(bring_forward(&mut desk), Ok(None));
        assert!(desk.tried.is_empty());
    }

    #[test]
    fn the_second_way_is_tried_when_the_first_is_refused() {
        let mut desk = Script::new();
        desk.answers = vec![(Rung::Plain, Tried::Refused)];
        desk.works = Some((Rung::Attached, 0));
        assert_eq!(bring_forward(&mut desk), Ok(Some(Rung::Attached)));
        assert_eq!(desk.tried, vec![Rung::Plain, Rung::Attached]);
        assert_eq!(desk.pauses, 0, "a refusal is not waited on");
    }

    #[test]
    fn an_accepted_call_without_the_window_in_front_is_a_failure() {
        // SetForegroundWindow returned TRUE and only flashed the taskbar
        // button: the way does not count, the next one is tried.
        let mut desk = Script::new();
        desk.answers = vec![(Rung::Plain, Tried::Sent)];
        desk.works = Some((Rung::AfterInput, 0));
        assert_eq!(bring_forward(&mut desk), Ok(Some(Rung::AfterInput)));
        assert_eq!(
            desk.tried,
            vec![Rung::Plain, Rung::Attached, Rung::AfterInput]
        );
        // Two ways were waited on to the end.
        assert_eq!(desk.pauses, 2 * (CHECKS - 1));
    }

    #[test]
    fn a_window_that_comes_forward_a_moment_later_counts() {
        let mut desk = Script::new();
        desk.works = Some((Rung::Plain, 3));
        assert_eq!(bring_forward(&mut desk), Ok(Some(Rung::Plain)));
        assert_eq!(desk.tried, vec![Rung::Plain]);
        assert_eq!(desk.pauses, 3);
    }

    #[test]
    fn a_way_that_does_not_apply_is_skipped_without_waiting() {
        let mut desk = Script::new();
        desk.answers = vec![
            (Rung::Plain, Tried::Refused),
            (Rung::Attached, Tried::Skipped),
            (Rung::AfterInput, Tried::Skipped),
        ];
        desk.works = Some((Rung::Switch, 1));
        assert_eq!(bring_forward(&mut desk), Ok(Some(Rung::Switch)));
        assert_eq!(desk.tried, LADDER.to_vec());
        assert_eq!(desk.pauses, 1);
    }

    #[test]
    fn every_way_failing_gives_one_error_about_the_likely_reason() {
        let mut desk = Script::new();
        assert_eq!(bring_forward(&mut desk), Err(FrontError::Refused));
        assert_eq!(desk.tried, LADDER.to_vec(), "all four, in order");
        let text = FrontError::Refused.text();
        assert!(text.contains("от имени администратора"), "{text}");
        // Either window may be the administrator's one.
        assert!(text.contains("окно, которое сейчас впереди"), "{text}");
    }

    #[test]
    fn no_input_is_sent_when_it_is_not_allowed() {
        // Keys held by the user, or a window of the blind zone in front.
        let mut desk = Script::new();
        desk.no_input = true;
        desk.works = Some((Rung::AfterInput, 0));
        assert_eq!(bring_forward(&mut desk), Err(FrontError::Refused));
        assert_eq!(desk.tried, vec![Rung::Plain, Rung::Attached, Rung::Switch]);
    }

    #[test]
    fn a_closed_window_fails_at_once() {
        let mut desk = Script::new();
        desk.alive = false;
        assert_eq!(bring_forward(&mut desk), Err(FrontError::Closed));
        assert!(desk.tried.is_empty());
        assert_eq!(desk.restored, 0, "a closed window is not touched");
        assert_eq!(FrontError::Closed.text(), "окно закрыто");
    }

    #[test]
    fn a_window_closed_on_the_way_is_reported_as_closed() {
        let mut desk = Script::new();
        desk.closes_at = Some(Rung::Attached);
        assert_eq!(bring_forward(&mut desk), Err(FrontError::Closed));
        assert_eq!(desk.tried, vec![Rung::Plain, Rung::Attached]);
    }

    #[test]
    fn the_whole_ladder_fits_the_time_budget() {
        // Four ways, each looked at CHECKS times with a pause in between.
        let mut desk = Script::new();
        let _ = bring_forward(&mut desk);
        assert_eq!(desk.pauses, 4 * (CHECKS - 1));
        let worst = FRONT_POLL * desk.pauses;
        assert!(worst <= std::time::Duration::from_millis(1500), "{worst:?}");
    }

    fn peer(thread: u32, answers: bool) -> Peer {
        Peer { thread, answers }
    }

    #[test]
    fn input_is_joined_to_the_window_in_front_and_to_the_wanted_one() {
        let plan = attach_plan(1, Some(peer(7, true)), peer(9, true)).unwrap();
        assert_eq!(plan.threads, vec![7, 9]);
        assert!(plan.raise);
        // One thread owns both windows: joined once.
        let plan = attach_plan(1, Some(peer(7, true)), peer(7, true)).unwrap();
        assert_eq!(plan.threads, vec![7]);
        // Nothing in front: the wanted window alone.
        let plan = attach_plan(1, None, peer(9, true)).unwrap();
        assert_eq!(plan.threads, vec![9]);
    }

    #[test]
    fn a_window_that_does_not_answer_is_neither_joined_nor_raised() {
        // The wanted window hangs: its thread stays out and it is not raised.
        let plan = attach_plan(1, Some(peer(7, true)), peer(9, false)).unwrap();
        assert_eq!(plan.threads, vec![7]);
        assert!(!plan.raise);
        // The window in front hangs: only the wanted one is joined.
        let plan = attach_plan(1, Some(peer(7, false)), peer(9, true)).unwrap();
        assert_eq!(plan.threads, vec![9]);
        assert!(plan.raise);
        // Both hang: the way does not apply.
        assert_eq!(attach_plan(1, Some(peer(7, false)), peer(9, false)), None);
        assert_eq!(attach_plan(1, None, peer(9, false)), None);
    }

    #[test]
    fn this_thread_is_never_joined_to_itself() {
        assert_eq!(attach_plan(1, Some(peer(1, true)), peer(1, true)), None);
        let plan = attach_plan(1, Some(peer(7, true)), peer(1, true)).unwrap();
        assert_eq!(plan.threads, vec![7]);
        // A window without a thread is a window that is gone.
        assert_eq!(attach_plan(1, Some(peer(0, true)), peer(0, true)), None);
    }

    #[test]
    fn a_dialog_the_program_put_forward_counts_as_the_program_in_front() {
        assert!(is_in_front(Sight {
            own: true,
            ..Sight::default()
        }));
        // A modal dialog the window does not own came forward in its place.
        assert!(is_in_front(Sight {
            same_thread: true,
            came: true,
            ..Sight::default()
        }));
        // The window is held by a modal dialog that was in front already.
        assert!(is_in_front(Sight {
            same_thread: true,
            blocked: true,
            ..Sight::default()
        }));
        // Another window of the same program that was in front all along.
        assert!(!is_in_front(Sight {
            same_thread: true,
            ..Sight::default()
        }));
        // A window of another program came forward meanwhile.
        assert!(!is_in_front(Sight {
            came: true,
            blocked: true,
            ..Sight::default()
        }));
    }
}
