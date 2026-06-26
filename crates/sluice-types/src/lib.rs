//! Sluice — the verdict/feed value types shared with the UI.
//!
//! The small, UI-agnostic types the Tauri app maps engine events into: the connection value type
//! (`pb::Connection`), the feed event model, and the verdict enums. (The engine's wire contract
//! lives in `sluice-proto`; this crate has no gRPC.)

mod conn;

pub mod decision;
pub mod feed;
pub mod verdict;

/// The connection value type the UI renders (`pb::Connection`).
pub use conn::pb;

pub use decision::{Action, Decision, RuleDuration, Scope};
pub use feed::{ConnState, FeedEvent};
