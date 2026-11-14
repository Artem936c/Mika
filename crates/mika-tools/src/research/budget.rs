//! Limits of one run: tokens, estimated, and time.

use std::time::{Duration, Instant};

use mika_core::llm::stream_cap;

use super::extract::{FACTS_PER_PAGE, FACT_CHARS};
use super::write::WRITE_CHARS;

/// Characters of the report prompt besides the facts: the rules, the question
/// and the source numbers.
const WRITE_OVERHEAD_CHARS: usize = 2500;
/// Longest time kept for the report.
const WRITE_TIME: Duration = Duration::from_secs(60);

/// Tokens of a text, estimated: three characters a token. Providers return no
/// exact count.
pub fn tokens(chars: usize) -> u32 {
    u32::try_from(chars.div_ceil(3)).unwrap_or(u32::MAX)
}

/// Tokens held for the output of a request whose answer is read up to
/// `max_chars` characters: the answer and the reasoning a model may write
/// beside it. Reading stops there, so a request cannot cost more output.
pub fn output_hold(max_chars: usize) -> u32 {
    tokens(stream_cap(max_chars))
}

/// Tokens kept for the report of a run that has sent `pages` pages to the
/// model: every fact of every page goes in, a whole report comes out.
pub fn write_reserve(pages: u32) -> u32 {
    let facts = pages as usize * FACTS_PER_PAGE * FACT_CHARS;
    tokens(facts + WRITE_OVERHEAD_CHARS).saturating_add(output_hold(WRITE_CHARS))
}

/// Tokens of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    max: u32,
    used: u32,
}

impl Budget {
    pub fn new(max: u32) -> Self {
        Self { max, used: 0 }
    }

    pub fn used(&self) -> u32 {
        self.used
    }

    pub fn left(&self) -> u32 {
        self.max.saturating_sub(self.used)
    }

    /// Whether a request of `need` tokens still fits.
    pub fn fits(&self, need: u32) -> bool {
        need <= self.left()
    }

    pub fn spend(&mut self, n: u32) {
        self.used = self.used.saturating_add(n);
    }

    /// Gives back tokens that were held for a request and not used.
    pub fn refund(&mut self, n: u32) {
        self.used = self.used.saturating_sub(n);
    }
}

/// Time of one run.
#[derive(Debug, Clone)]
pub struct Clock {
    start: Instant,
    total: Duration,
}

impl Clock {
    pub fn new(minutes: u32) -> Self {
        Self::lasting(Duration::from_secs(u64::from(minutes) * 60))
    }

    /// A clock of any length.
    pub fn lasting(total: Duration) -> Self {
        Self {
            start: Instant::now(),
            total,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// Time left of the whole run; zero when it is over.
    pub fn left(&self) -> Duration {
        self.total.saturating_sub(self.elapsed())
    }

    /// Time kept for the report: a minute, but no more than a third of the run.
    pub fn write_reserve(&self) -> Duration {
        WRITE_TIME.min(self.total / 3)
    }

    /// Time left for searching and reading; zero when it is over.
    pub fn collect_left(&self) -> Duration {
        self.left().saturating_sub(self.write_reserve())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_characters_a_token() {
        assert_eq!(tokens(0), 0);
        assert_eq!(tokens(1), 1);
        assert_eq!(tokens(3), 1);
        assert_eq!(tokens(4), 2);
        assert_eq!(tokens(9000), 3000);
        assert_eq!(tokens(usize::MAX), u32::MAX);
    }

    #[test]
    fn report_reserve_grows_with_the_pages() {
        // 8 pages: (8 * 6 * 300 + 2500) / 3 for the facts; 9000 characters of
        // answer and as much of reasoning for what comes out.
        assert_eq!(write_reserve(8), 5634 + 6000);
        assert_eq!(write_reserve(2), 2034 + 6000);
        assert!(write_reserve(16) > write_reserve(8));
        assert!(write_reserve(16) < 20_000);
    }

    #[test]
    fn output_is_held_with_room_for_reasoning() {
        // The answer and at least 3000 characters of reasoning.
        assert_eq!(output_hold(800), tokens(3800));
        assert_eq!(output_hold(2000), tokens(5000));
        assert_eq!(output_hold(9000), 6000);
    }

    #[test]
    fn budget_counts_spent_and_held_tokens() {
        let mut b = Budget::new(1000);
        assert_eq!((b.used(), b.left()), (0, 1000));
        assert!(b.fits(1000) && !b.fits(1001));
        b.spend(400);
        assert_eq!((b.used(), b.left()), (400, 600));
        assert!(b.fits(600) && !b.fits(601));
        b.refund(150);
        assert_eq!((b.used(), b.left()), (250, 750));
        b.spend(5000);
        assert_eq!((b.used(), b.left()), (5250, 0));
        assert!(b.fits(0) && !b.fits(1));
        b.refund(u32::MAX);
        assert_eq!(b.used(), 0);
        b.spend(u32::MAX);
        b.spend(u32::MAX);
        assert_eq!(b.used(), u32::MAX);
    }

    #[test]
    fn clock_keeps_time_for_the_report() {
        let five = Clock::new(5);
        assert_eq!(five.write_reserve(), Duration::from_secs(60));
        assert!(five.left() <= Duration::from_secs(300));
        assert!(five.left() > Duration::from_secs(295));
        assert!(five.collect_left() <= Duration::from_secs(240));
        assert!(five.collect_left() > Duration::from_secs(235));
        // A short run keeps a third.
        let two = Clock::new(2);
        assert_eq!(two.write_reserve(), Duration::from_secs(40));
        assert!(two.collect_left() <= Duration::from_secs(80));
    }

    #[test]
    fn clock_stops_at_zero() {
        let over = Clock::lasting(Duration::ZERO);
        assert_eq!(over.left(), Duration::ZERO);
        assert_eq!(over.collect_left(), Duration::ZERO);
        let short = Clock::lasting(Duration::from_millis(30));
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(short.left(), Duration::ZERO);
        assert!(short.elapsed() >= Duration::from_millis(30));
    }
}
