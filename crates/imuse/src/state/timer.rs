//! Port of `timer.c`, `internal/timer.h`. The cadence is fixed, so there is no state.

/// The original services iMUSE every two ~4 ms PIT interrupts.
#[doc(alias = "IM_SERVICE_PERIOD_US")]
pub(crate) const SERVICE_PERIOD_US: i32 = 8000;
/// Each service advances the logical clock by the host's nominal 8060 µs.
#[doc(alias = "IM_LOGICAL_TICK_US")]
pub(crate) const LOGICAL_TICK_US: i32 = 8060;
/// With X-Wing timing both the service period and the logical tick are the DOS93 PIT's 3433 µs.
pub(crate) const XWING_TICK_US: i32 = 3433;
