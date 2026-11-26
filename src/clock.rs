//! The clock the interpreter and the lowering's phase timers read:
//! `std::time`'s natively. wasm32-unknown-unknown's std has none
//! (`Instant::now()` panics "time not implemented on this platform"), so
//! the browser playground reads the browser's through `web-time`, whose
//! types have the same API (B-2026-11-25-57).

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::{Instant, SystemTime, UNIX_EPOCH};
#[cfg(target_arch = "wasm32")]
pub use web_time::{Instant, SystemTime, UNIX_EPOCH};
