//! Reusable Blink Rust API. This experimental interpreter has no C fallback.
pub use blink_core::{Error, Result};
pub use blink_runtime::{
    BoundedOutput, CancellationToken, Command, GuestArch, RunOutcome, Runtime as Blink,
    RuntimeConfig, Termination,
};
