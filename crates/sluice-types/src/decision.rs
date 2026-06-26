//! Verdict value types shared with the UI.
//!
//! These are the typed forms of a firewall verdict — what to do ([`Action`]), how widely
//! ([`Scope`]), and for how long ([`RuleDuration`]) — bundled as a [`Decision`]. UI-agnostic.

/// What verdict to enforce. The proto carries these as strings; this is the typed form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Allow,
    Deny,
    Reject,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Allow => "allow",
            Action::Deny => "deny",
            Action::Reject => "reject",
        }
    }

    /// True for the only non-blocking verdict.
    pub fn is_allow(self) -> bool {
        matches!(self, Action::Allow)
    }
}

/// How widely a decision applies — the scope a user picks in the two-click flow (FR-011).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// This exact connection: app + host + port (+ protocol).
    Connection,
    /// This app → this host (any port).
    AppToHost,
    /// This app → any host.
    AppToAny,
    /// This host → any app.
    HostToAny,
}

/// How long a decision lasts (FR-012). `Always` is the only one the daemon persists to disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleDuration {
    Once,
    UntilRestart,
    Always,
}

impl RuleDuration {
    pub fn as_str(self) -> &'static str {
        match self {
            RuleDuration::Once => "once",
            RuleDuration::UntilRestart => "until restart",
            RuleDuration::Always => "always",
        }
    }
}

/// A resolved verdict: what to do, at what scope, for how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub action: Action,
    pub scope: Scope,
    pub duration: RuleDuration,
}
