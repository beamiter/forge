//! Core for the Forge ASCII organism prototype.
//!
//! The life engine deliberately has no GTK, VTE, PTY, network, or LLM
//! dependency. The executable is only a temporary body/event harness; Forge
//! can later feed the same [`LifeEvent`] values from its native command and
//! input events.

pub mod life;
pub mod memory;

pub use life::{Behavior, LifeEvent, LifeState, Organism, Reaction};
pub use memory::{DailyStats, Memory};
