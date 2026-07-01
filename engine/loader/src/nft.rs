//! Inbound enforcement via nftables (E6.1). The engine manages a dedicated `inet sluice` table
//! with a default-deny `input` chain; `established,related` + loopback are always accepted
//! (structural — dropping those would kill replies to your own outbound), then the user's
//! allow-list, then drop. Off by default (observe); the UI toggles it.
//!
//! Programs nftables by piping an atomic ruleset to `nft -f -` (root). The table is removed on
//! stop/exit and on startup (clearing any stale table from a crash), so stopping the engine
//! reopens inbound — recovery is `sudo systemctl stop sluice-engine` (or `nft delete table inet
//! sluice`). NOTE: this filters the host's own INPUT; Docker-published ports traverse FORWARD,
//! not INPUT, so they aren't affected (a later concern).

use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
};

use serde::{Deserialize, Serialize};

/// An allowed inbound port.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AllowPort {
    pub proto: String, // "tcp" | "udp"
    pub port: u16,
}

/// Persisted inbound posture.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct InboundConfig {
    #[serde(default)]
    pub enforce: bool,
    #[serde(default)]
    pub allow: Vec<AllowPort>,
}

/// Owns the inbound config + its on-disk mirror, and drives the nftables table.
pub struct Inbound {
    cfg: InboundConfig,
    path: PathBuf,
}

impl Inbound {
    pub fn load(path: PathBuf) -> Self {
        let cfg = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { cfg, path }
    }

    pub fn get(&self) -> InboundConfig {
        self.cfg.clone()
    }

    /// On startup: clear any stale table, then enforce if the saved posture says so.
    pub fn startup(&self) {
        let _ = teardown();
        if self.cfg.enforce {
            match apply(&self.cfg) {
                Ok(()) => eprintln!(
                    "[sluice] inbound ENFORCING (default-deny + {} allow rule(s))",
                    self.cfg.allow.len()
                ),
                Err(e) => eprintln!("[sluice] WARN: inbound enforce failed: {e}"),
            }
        } else {
            eprintln!("[sluice] inbound observe (enforcement off)");
        }
    }

    /// Replace the posture: persist + apply (or tear down).
    pub fn set(&mut self, cfg: InboundConfig) -> Result<(), String> {
        // Apply first so a bad ruleset doesn't get persisted as the active posture.
        if cfg.enforce {
            apply(&cfg).map_err(|e| e.to_string())?;
        } else {
            teardown().map_err(|e| e.to_string())?;
        }
        self.cfg = cfg;
        self.save().map_err(|e| e.to_string())?;
        eprintln!(
            "[sluice] inbound {} ({} allow rule(s))",
            if self.cfg.enforce {
                "ENFORCING"
            } else {
                "observe"
            },
            self.cfg.allow.len()
        );
        Ok(())
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(&self.cfg).unwrap_or_else(|_| "{}".into());
        std::fs::write(&self.path, json)?;
        std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}

/// Remove the engine's nftables table — call on exit so inbound reopens.
pub fn teardown() -> std::io::Result<()> {
    // "add then delete" so it never errors whether or not the table currently exists.
    nft("add table inet sluice\ndelete table inet sluice\n")
}

/// Apply the default-deny input chain with the config's allow-list (atomic replace).
fn apply(cfg: &InboundConfig) -> std::io::Result<()> {
    let mut tcp = Vec::new();
    let mut udp = Vec::new();
    for a in &cfg.allow {
        if a.proto.eq_ignore_ascii_case("udp") {
            udp.push(a.port)
        } else {
            tcp.push(a.port)
        }
    }
    let ports = |v: &[u16]| {
        v.iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut rules = String::new();
    rules.push_str("    ct state established,related accept\n");
    rules.push_str("    iif \"lo\" accept\n");
    rules.push_str("    ct state invalid drop\n");
    if !tcp.is_empty() {
        rules.push_str(&format!("    tcp dport {{ {} }} accept\n", ports(&tcp)));
    }
    if !udp.is_empty() {
        rules.push_str(&format!("    udp dport {{ {} }} accept\n", ports(&udp)));
    }
    // Log packets about to be dropped to NFLOG so the UI can show BLOCKED inbound (#23). `log` is
    // non-terminating and only accepted packets leave the chain above, so exactly the to-be-dropped
    // packets are logged, then the chain's `policy drop` drops them (unchanged).
    rules.push_str(&format!(
        "    counter log group {}\n",
        crate::nflog::NFLOG_GROUP
    ));

    // Atomic replace: add (ensures exists) → delete → add fresh, all in one `nft -f` transaction.
    let ruleset = format!(
        "add table inet sluice\n\
         delete table inet sluice\n\
         add table inet sluice {{\n\
         \x20 chain input {{\n\
         \x20   type filter hook input priority 0; policy drop;\n\
         {rules}\
         \x20 }}\n\
         }}\n"
    );
    nft(&ruleset)
}

/// Pipe a ruleset to `nft -f -`.
fn nft(ruleset: &str) -> std::io::Result<()> {
    let mut child = Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("nft stdin")
        .write_all(ruleset.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "nft failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}
