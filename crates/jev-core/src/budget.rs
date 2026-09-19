//! Call and token accounting. Checked *before* a call, never after
//! (PROTOCOL.md §5, docs/MODEL.md §7).

use crate::config::BudgetConfig;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    PerMinute,
    PerHour,
    Tokens,
    InFlight,
}

impl Refusal {
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::PerMinute => "per-minute call budget reached",
            Refusal::PerHour => "per-hour call budget reached",
            Refusal::Tokens => "session token budget reached",
            Refusal::InFlight => "too many calls already in flight",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permit {
    Granted,
    Refused(Refusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub calls_last_minute: u32,
    pub calls_last_hour: u32,
    pub tokens_used: u64,
    pub in_flight: u32,
}

struct Inner {
    minute: VecDeque<Instant>,
    hour: VecDeque<Instant>,
    tokens: u64,
    in_flight: u32,
}

pub struct Budget {
    inner: Mutex<Inner>,
    max_in_flight: u32,
    hits: Mutex<u64>,
    refusals: Mutex<u64>,
}

impl Budget {
    pub fn new(max_in_flight: u32) -> Self {
        Budget {
            inner: Mutex::new(Inner {
                minute: VecDeque::new(),
                hour: VecDeque::new(),
                tokens: 0,
                in_flight: 0,
            }),
            max_in_flight,
            hits: Mutex::new(0),
            refusals: Mutex::new(0),
        }
    }

    fn prune(inner: &mut Inner, now: Instant) {
        let minute = Duration::from_secs(60);
        let hour = Duration::from_secs(3600);
        while inner.minute.front().is_some_and(|t| now.duration_since(*t) >= minute) {
            inner.minute.pop_front();
        }
        while inner.hour.front().is_some_and(|t| now.duration_since(*t) >= hour) {
            inner.hour.pop_front();
        }
    }

    fn acquire_at(&self, cfg: &BudgetConfig, now: Instant) -> Permit {
        let mut inner = self.inner.lock();
        Self::prune(&mut inner, now);

        let refusal = if inner.in_flight >= self.max_in_flight {
            Some(Refusal::InFlight)
        } else if inner.minute.len() as u32 >= cfg.max_calls_per_min {
            Some(Refusal::PerMinute)
        } else if inner.hour.len() as u32 >= cfg.max_calls_per_hour {
            Some(Refusal::PerHour)
        } else if cfg.max_tokens_per_session > 0 && inner.tokens >= cfg.max_tokens_per_session {
            Some(Refusal::Tokens)
        } else {
            None
        };

        if let Some(r) = refusal {
            *self.refusals.lock() += 1;
            return Permit::Refused(r);
        }

        inner.minute.push_back(now);
        inner.hour.push_back(now);
        inner.in_flight += 1;
        *self.hits.lock() += 1;
        Permit::Granted
    }

    /// Check out a permit. Refusal is a normal outcome, not an error.
    pub fn try_acquire(&self, cfg: &BudgetConfig) -> Permit {
        self.acquire_at(cfg, Instant::now())
    }

    pub fn release(&self) {
        let mut inner = self.inner.lock();
        inner.in_flight = inner.in_flight.saturating_sub(1);
    }

    pub fn record_tokens(&self, n: u64) {
        let mut inner = self.inner.lock();
        inner.tokens = inner.tokens.saturating_add(n);
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        let mut inner = self.inner.lock();
        Self::prune(&mut inner, Instant::now());
        BudgetSnapshot {
            calls_last_minute: inner.minute.len() as u32,
            calls_last_hour: inner.hour.len() as u32,
            tokens_used: inner.tokens,
            in_flight: inner.in_flight,
        }
    }

    pub fn counters(&self) -> (u64, u64) {
        (
            *self.hits.lock(),
            *self.refusals.lock(),
        )
    }

    /// Limits as configured, for `:Jev status`.
    pub fn limits(cfg: &BudgetConfig) -> (u32, u32, u64) {
        (
            cfg.max_calls_per_min,
            cfg.max_calls_per_hour,
            cfg.max_tokens_per_session,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BudgetConfig {
        BudgetConfig {
            max_calls_per_min: 2,
            max_calls_per_hour: 3,
            max_tokens_per_session: 100,
            timeout_ms: 1000,
        }
    }

    #[test]
    fn permits_are_granted_until_the_minute_cap_then_refused() {
        let b = Budget::new(8);
        let t0 = Instant::now();
        assert_eq!(b.acquire_at(&cfg(), t0), Permit::Granted);
        assert_eq!(b.acquire_at(&cfg(), t0), Permit::Granted);
        assert_eq!(
            b.acquire_at(&cfg(), t0),
            Permit::Refused(Refusal::PerMinute)
        );
    }

    #[test]
    fn the_minute_window_slides() {
        let b = Budget::new(8);
        let t0 = Instant::now();
        b.acquire_at(&cfg(), t0);
        b.acquire_at(&cfg(), t0);
        assert_eq!(
            b.acquire_at(&cfg(), t0),
            Permit::Refused(Refusal::PerMinute)
        );
        assert_eq!(
            b.acquire_at(&cfg(), t0 + Duration::from_secs(61)),
            Permit::Granted
        );
    }

    #[test]
    fn the_hour_cap_binds_even_when_the_minute_has_room() {
        let b = Budget::new(8);
        let t0 = Instant::now();
        // three calls spread far enough apart to clear the per-minute window
        b.acquire_at(&cfg(), t0);
        b.acquire_at(&cfg(), t0 + Duration::from_secs(61));
        b.acquire_at(&cfg(), t0 + Duration::from_secs(122));
        assert_eq!(
            b.acquire_at(&cfg(), t0 + Duration::from_secs(183)),
            Permit::Refused(Refusal::PerHour)
        );
    }

    #[test]
    fn the_token_cap_binds() {
        let b = Budget::new(8);
        let cfg = BudgetConfig {
            max_tokens_per_session: 10,
            ..cfg()
        };
        b.record_tokens(10);
        assert_eq!(
            b.try_acquire(&cfg),
            Permit::Refused(Refusal::Tokens)
        );
    }

    #[test]
    fn in_flight_is_bounded_and_released() {
        let b = Budget::new(1);
        let cfg = BudgetConfig {
            max_calls_per_min: 99,
            max_calls_per_hour: 99,
            ..cfg()
        };
        assert_eq!(b.try_acquire(&cfg), Permit::Granted);
        assert_eq!(b.try_acquire(&cfg), Permit::Refused(Refusal::InFlight));
        b.release();
        assert_eq!(b.try_acquire(&cfg), Permit::Granted);
    }

    #[test]
    fn snapshot_reports_what_was_spent() {
        let b = Budget::new(8);
        let cfg = cfg();
        b.try_acquire(&cfg);
        b.record_tokens(7);
        let s = b.snapshot();
        assert_eq!(s.calls_last_minute, 1);
        assert_eq!(s.tokens_used, 7);
        assert_eq!(s.in_flight, 1);
        assert_eq!(Budget::limits(&cfg), (2, 3, 100));
    }
}
