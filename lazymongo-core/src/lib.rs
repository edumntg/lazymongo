//! lazymongo-core: state, types, and the MongoDB I/O actor.
//!
//! This crate has no TUI dependencies. The TUI talks to MongoDB exclusively
//! through the [`actor`] message channels, keeping all network I/O off the
//! render path.

pub mod actor;
pub mod query;
pub mod types;

/// Re-export the driver's bson so every crate uses the exact same version.
pub use mongodb::bson;
