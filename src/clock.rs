//! The current instant: the system clock that `fn:current-dateTime` reads,
//! and the one instant the XPath expressions of a validation run share.
//!
//! An XPath evaluation reads the clock once, the first time its expression
//! asks for the current dateTime, and keeps it (F&O 1.0 §16.3: the function
//! "is ·stable·"). A validation run evaluates many expressions — every XSD 1.1
//! assertion, assertion facet and type alternative it meets — each in a
//! dynamic context of its own, and XSD 1.1 Structures §3.13.4.2 clause 6
//! wants them to agree: "The current dateTime is ·implementation-defined·, but
//! is constant during an ·assessment· episode." So the run owns a [`SharedClock`]
//! and lends it, explicitly, to every evaluation it makes.

// Without `xsd11` there is no XPath engine: nothing reads a clock, and the
// validation code passes `None` for the run's clock.
#![cfg_attr(not(feature = "xsd11"), allow(dead_code))]

use std::time::{SystemTime, UNIX_EPOCH};

/// An instant of the system clock: whole seconds since
/// 1970-01-01T00:00:00Z — negative before it — and the nanoseconds into that
/// second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostClock {
    /// Whole seconds since the Unix epoch, rounded towards negative infinity.
    pub(crate) seconds: i64,
    /// Nanoseconds into `seconds`, below 1,000,000,000.
    pub(crate) nanos: u32,
}

impl HostClock {
    /// `time` in seconds and nanoseconds since the Unix epoch. A time before
    /// 1970 gives a negative count of seconds, not a panic.
    pub(crate) fn from_system_time(time: SystemTime) -> Self {
        match time.duration_since(UNIX_EPOCH) {
            Ok(after) => Self {
                seconds: i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
                nanos: after.subsec_nanos(),
            },
            Err(before) => {
                let before = before.duration();
                let seconds = i64::try_from(before.as_secs()).unwrap_or(i64::MAX);
                match before.subsec_nanos() {
                    0 => Self {
                        seconds: -seconds,
                        nanos: 0,
                    },
                    nanos => Self {
                        seconds: -seconds - 1,
                        nanos: 1_000_000_000 - nanos,
                    },
                }
            }
        }
    }

    /// The system clock now; `None` on `wasm32-unknown-unknown` (any
    /// `wasm*-unknown-unknown`), whose standard library has no clock —
    /// `SystemTime::now()` panics there. A host on that target gives an XPath
    /// evaluation its current dateTime (`DynamicContext::current_datetime`)
    /// and a validation run its instant (`ValidationRuntime::set_current_time`).
    pub(crate) fn now() -> Option<Self> {
        #[cfg(all(target_family = "wasm", target_os = "unknown"))]
        {
            None
        }
        #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
        {
            Some(Self::from_system_time(SystemTime::now()))
        }
    }
}

/// One instant shared by a group of XPath evaluations: those of one validation
/// run (XSD 1.1 Structures §3.13.4.2 clause 6), or the schema-level values of
/// one schema set.
///
/// - A validation runtime owns one per run and passes a reference to every
///   call that can evaluate an assertion or a type alternative, down to the
///   dynamic contexts. The instant is the host's when it sets one
///   (`ValidationRuntime::set_current_time`, which replaces the clock), or the
///   system clock read the first time an expression of the run asks.
/// - A schema set owns one for the values it reads from its own literals —
///   an enumeration's values, a value constraint's {value}, the literals its
///   schema-time checks compare — which are schema properties: each is read
///   at the schema set's instant, the system clock read the first time one of
///   them needs it, so that within one schema set a literal reads the same on
///   every path and in every run.
///
/// A group that evaluates no `fn:current-dateTime` reads no clock.
#[derive(Debug, Default)]
pub(crate) struct SharedClock {
    instant: std::sync::OnceLock<Option<HostClock>>,
}

impl SharedClock {
    /// A clock that has read nothing yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A clock whose instant is `instant` (the host's).
    pub(crate) fn fixed(instant: HostClock) -> Self {
        let clock = Self::new();
        let _ = clock.instant.set(Some(instant));
        clock
    }

    /// Fix the instant to `instant` unless it was read or fixed already;
    /// `false` then.
    #[cfg(test)]
    pub(crate) fn fix(&self, instant: HostClock) -> bool {
        self.instant.set(Some(instant)).is_ok()
    }

    /// The instant: the one fixed or read before, else the system clock now,
    /// which the clock then keeps. `None` only on a target without a clock.
    pub(crate) fn instant(&self) -> Option<HostClock> {
        *self.instant.get_or_init(HostClock::now)
    }

    /// The instant fixed or read so far, without reading the clock.
    #[cfg(test)]
    pub(crate) fn peek(&self) -> Option<HostClock> {
        self.instant.get().copied().flatten()
    }
}

impl Clone for SharedClock {
    /// A clock with the instant this one has so far (none, if it has not read
    /// one yet).
    fn clone(&self) -> Self {
        match self.instant.get() {
            Some(&instant) => {
                let clock = Self::new();
                let _ = clock.instant.set(instant);
                clock
            }
            None => Self::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Offsets in whole 100 ns and instants after 1601 only: what `SystemTime`
    /// holds on every platform (Windows keeps 100 ns intervals since
    /// 1601-01-01). The single nanosecond and the year 1 are
    /// `datetime_from_unix`'s cases, whose inputs are integers.
    #[test]
    fn host_clock_counts_from_the_epoch_in_both_directions() {
        let at = |time| {
            let clock = HostClock::from_system_time(time);
            (clock.seconds, clock.nanos)
        };
        assert_eq!(at(UNIX_EPOCH), (0, 0));
        assert_eq!(at(UNIX_EPOCH + Duration::new(1, 500)), (1, 500));
        assert_eq!(
            at(UNIX_EPOCH + Duration::new(1_764_000_000, 123_456_700)),
            (1_764_000_000, 123_456_700)
        );
        // Before 1970: whole seconds floored, nanoseconds forward — no panic.
        assert_eq!(at(UNIX_EPOCH - Duration::from_secs(1)), (-1, 0));
        assert_eq!(
            at(UNIX_EPOCH - Duration::from_millis(1_500)),
            (-2, 500_000_000)
        );
        assert_eq!(
            at(UNIX_EPOCH - Duration::from_nanos(100)),
            (-1, 999_999_900)
        );
        // 1969-01-01T00:00:00Z.
        assert_eq!(
            at(UNIX_EPOCH - Duration::from_secs(31_536_000)),
            (-31_536_000, 0)
        );
    }

    /// A host's instant is kept, a second fix is refused, and no clock is read
    /// for it (this test runs under an isolated Miri, which refuses the system
    /// clock).
    #[test]
    fn a_fixed_instant_is_kept_and_reads_no_clock() {
        let fixed = HostClock {
            seconds: 946_684_800,
            nanos: 0,
        };
        let other = HostClock {
            seconds: 0,
            nanos: 0,
        };
        let clock = SharedClock::new();
        assert_eq!(clock.peek(), None);
        assert!(clock.fix(fixed));
        assert!(!clock.fix(other), "the instant is fixed once");
        assert_eq!(clock.instant(), Some(fixed));
        assert_eq!(clock.instant(), Some(fixed));
        assert_eq!(clock.peek(), Some(fixed));
        let host = SharedClock::fixed(fixed);
        assert_eq!(host.instant(), Some(fixed));
        assert_eq!(host.clone().instant(), Some(fixed));
        assert_eq!(SharedClock::new().clone().peek(), None);
    }
}
