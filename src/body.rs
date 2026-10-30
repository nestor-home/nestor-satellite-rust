//! What a driver implements to carry out the core's intents. Every method has a no-op default:
//! override what the device can do.
//!
//! Calls come from the network task and must return at once: keep the state, animate elsewhere
//! (a task or thread of the driver's own). Physical safety — limits, watchdogs — lives in the
//! driver and never depends on the link being up.

use crate::protocol::{Behavior, Expression};

/// The device's body, as the core drives it.
pub trait Body: Send + Sync + 'static {
    /// The assistant's attitude changed (idle, listening, thinking, speaking): show it until the next.
    fn behavior(&self, _behavior: Behavior) {}

    /// Perform an expression now (`intensity` between 0 and 1): it arrives as the matching audio
    /// is played.
    fn expression(&self, _expression: Expression, _intensity: f64) {}

    /// A word is being played: a light speech accent.
    fn beat(&self) {}

    /// The core was just heard from (a message or audio). A watchdog can rest the device when
    /// heartbeats stop.
    fn heartbeat(&self) {}

    /// The link to the core came up (welcomed) or went down.
    fn connected(&self, _up: bool) {}
}

/// A body that does nothing: a speaker without lights or motors.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoBody;

impl Body for NoBody {}
