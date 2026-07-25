//! Durable, scheduled, at-least-once actions over a single serialized state.
//!
//! An action may perform external side effects more than once: its state update
//! and completion are committed only after the handler returns successfully.

mod action;
mod engine;
mod error;
mod storage;

pub use action::{Action, ActionId, HandlerError};
pub use async_trait::async_trait;
pub use engine::{Builder, Handle, Runner};
pub use error::{Error, RunnerError};
