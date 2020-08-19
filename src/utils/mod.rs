pub mod message_error;

pub use message_error::*;

#[inline]
pub fn min_instant(lhs: std::time::Instant, rhs: std::time::Instant) -> std::time::Instant {
    if lhs < rhs {
        lhs
    }
    else {
        rhs
    }
}

#[inline]
pub fn max_instant(lhs: std::time::Instant, rhs: std::time::Instant) -> std::time::Instant {
    if lhs > rhs {
        lhs
    }
    else {
        rhs
    }
}