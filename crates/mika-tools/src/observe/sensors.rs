//! What the watcher asks the system. The real answers are in `win/sensors.rs`;
//! tests give their own.

pub use mika_core::triggers::{AudioDevice, SessionState};

/// One process of the user's session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    /// Executable name, lower case: `chrome.exe`.
    pub name: String,
    /// Unix seconds when it started; 0 = unknown.
    pub started: u64,
}

/// Every method blocks: callers run them in `tokio::task::spawn_blocking`.
pub trait Sensors: Send + Sync {
    /// Active sound devices, output and input.
    fn audio_devices(&self) -> Result<Vec<AudioDevice>, String>;
    /// Processes of the user's own Windows session only.
    fn processes(&self) -> Result<Vec<Proc>, String>;
    /// The user's own session.
    fn session(&self) -> Result<SessionState, String>;
    /// Seconds since the user's last key press or mouse move in this session.
    /// Keys and clicks Mika sends herself do not count (`OwnInput`): a
    /// scenario that presses a media key must not look like a return.
    fn idle_seconds(&self) -> u64;
    /// Text of a window as `ui_read` sees it: name and value of every element,
    /// one per line. The caller checks the "blind zone" first.
    fn window_text(&self, window: u64) -> Result<String, String>;
}

/// How long after her own input Windows may still stamp it, milliseconds.
const OWN_SLACK_MS: u32 = 500;

/// Tells the user's input from the input Mika sends herself.
///
/// Windows keeps one "last input" time for the session, and a key she sends
/// moves it like a key the user pressed. So every input of hers is wrapped:
/// `begin` just before it notes the user's last input so far, `end` just
/// after it notes when hers was sent. `user_input` then gives the user's own
/// last input, whatever she sent since. Times are `GetTickCount` values
/// (milliseconds, wrapping), so the arithmetic is tested without Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct OwnInput {
    /// The user's last input known when she last sent something.
    real: u32,
    /// When she last sent input.
    sent: u32,
    /// She has sent input at all.
    any: bool,
}

/// `a` is later than `b`, for tick counts that wrap.
fn later(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < u32::MAX / 2
}

impl OwnInput {
    pub(crate) const fn new() -> Self {
        Self {
            real: 0,
            sent: 0,
            any: false,
        }
    }

    /// The last input of the session is hers, not the user's.
    fn own(&self, last_input: u32) -> bool {
        self.any && !later(last_input, self.sent.wrapping_add(OWN_SLACK_MS))
    }

    /// She is about to send input; `last_input` is the session's last input now.
    pub(crate) fn begin(&mut self, last_input: u32) {
        if !self.own(last_input) {
            self.real = last_input;
        }
    }

    /// She has sent her input; `now` is the tick count.
    pub(crate) fn end(&mut self, now: u32) {
        self.sent = now;
        self.any = true;
    }

    /// The user's own last input, given the session's last input.
    pub(crate) fn user_input(&self, last_input: u32) -> u32 {
        if self.own(last_input) {
            self.real
        } else {
            last_input
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn her_own_input_does_not_count_as_the_users() {
        let mut own = OwnInput::new();
        // Nothing sent yet: the session's last input is the user's.
        assert_eq!(own.user_input(5_000), 5_000);
        // The user typed at 10 s and left. Ten minutes later a scenario
        // presses a media key.
        own.begin(10_000);
        own.end(610_020);
        // Windows now says "last input at 610 s": it is hers.
        assert_eq!(own.user_input(610_010), 10_000);
        assert_eq!(own.user_input(610_020 + OWN_SLACK_MS), 10_000);
        // A second key of hers a minute later changes nothing either.
        own.begin(610_010);
        own.end(670_000);
        assert_eq!(own.user_input(669_990), 10_000);
        // The user comes back: an input after hers is the user's.
        assert_eq!(own.user_input(700_000), 700_000);
        // And it is remembered when she sends something again.
        own.begin(700_000);
        own.end(701_000);
        assert_eq!(own.user_input(701_000), 700_000);
    }

    #[test]
    fn input_she_could_not_send_changes_nothing() {
        let mut own = OwnInput::new();
        // Windows refused her input: the last input stays the user's old one.
        own.begin(10_000);
        own.end(20_000);
        assert_eq!(own.user_input(10_000), 10_000);
    }

    #[test]
    fn tick_counts_may_wrap() {
        let mut own = OwnInput::new();
        let user = u32::MAX - 5_000;
        own.begin(user);
        own.end(u32::MAX - 100);
        // Her input, stamped just before the counter wrapped.
        assert_eq!(own.user_input(u32::MAX - 150), user);
        // The user's next input comes after the wrap.
        assert_eq!(own.user_input(4_000), 4_000);
        assert!(later(4_000, u32::MAX - 100) && !later(u32::MAX - 100, 4_000));
        assert!(!later(7, 7));
    }
}
