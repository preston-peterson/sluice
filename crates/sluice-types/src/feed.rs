//! The live feed model — the data the UI's home screen renders (FR-001..004).
//!
//! A [`FeedEvent`] carries one connection the engine observed plus its verdict and a human
//! "why". The UI maps engine connection events into these and renders them.

use crate::decision::{Action, RuleDuration, Scope};
use crate::pb;

/// Visual state of a feed row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Pending,
    Allowed,
    Blocked,
}

/// One update about a connection. Cloneable so it can fan out over a broadcast channel.
#[derive(Clone, Debug)]
pub struct FeedEvent {
    /// Stable id linking the `Pending` event to its later resolution.
    pub id: u64,
    pub conn: pb::Connection,
    pub state: ConnState,
    /// The verdict, once resolved (None while `Pending`).
    pub action: Option<Action>,
    pub scope: Option<Scope>,
    pub duration: Option<RuleDuration>,
    /// Human-readable cause: "pending", "user decision", "default action (timeout)", etc.
    pub why: String,
    /// Wall-clock time, ms since the Unix epoch.
    pub at_unix_ms: u128,
    /// Container the process runs in (e.g. "uptime-kuma"), empty if none / not known. Set by
    /// the Sluice engine path.
    pub container: String,
    /// True for an INCOMING connection (remote → local), observed by the engine (E6.0). For
    /// inbound rows `conn.dst_ip` is the remote peer and `conn.dst_port` is the local port.
    pub inbound: bool,
}
