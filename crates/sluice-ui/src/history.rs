//! Sluice-owned local history store (DEC-005: SQLite). Persists resolved feed decisions so the
//! feed/counts survive restarts and (later) feed time-window views. This is **separate from the
//! engine's rule store** (AR-5): wiping one never corrupts the other, and it lives under the
//! user's data dir, not a privileged/system location.
//!
//! Only Sluice's own connection-decision records live here — no engine rules, nothing privileged.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rusqlite::{params, Connection};

use crate::FeedRow;

/// Keep at most this many rows; older ones are pruned on open.
const MAX_ROWS: i64 = 50_000;
/// Security events are lower-volume; keep a smaller rolling window.
const MAX_ALERTS: i64 = 10_000;

pub struct History {
    conn: Connection,
    /// True if backed by a real file (false = in-memory fallback, not persisted).
    pub persistent: bool,
}

impl History {
    /// Open the history DB at `path`, falling back to in-memory if the file can't be opened
    /// (so a bad data dir degrades gracefully rather than breaking the app).
    pub fn open(path: &Path) -> Self {
        match Connection::open(path) {
            Ok(conn) => {
                // Connection history is PII (hosts, paths, IPs) — owner-only at rest (SEC-009).
                // Covers the -journal/-wal sidecars too, since they share the 0700 data dir.
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
                let mut h = History {
                    conn,
                    persistent: true,
                };
                if let Err(e) = h.init() {
                    tracing::warn!(error = %e, "history: schema init failed; using in-memory");
                    return Self::in_memory();
                }
                h
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "history: open failed; using in-memory");
                Self::in_memory()
            }
        }
    }

    fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        let mut h = History {
            conn,
            persistent: false,
        };
        let _ = h.init();
        h
    }

    fn init(&mut self) -> rusqlite::Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at_ms INTEGER NOT NULL,
                process TEXT, host TEXT, dst_ip TEXT, port INTEGER,
                protocol TEXT, pid INTEGER, uid INTEGER,
                src_ip TEXT, src_port INTEGER,
                state TEXT, action TEXT, why TEXT,
                inbound INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_events_id ON events(id);
            CREATE TABLE IF NOT EXISTS alerts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                at_ms INTEGER NOT NULL,
                level TEXT, priority TEXT, what TEXT,
                text TEXT, process TEXT, host TEXT
            );",
        )?;
        // Migrate older DBs created before the `inbound` column existed (the CREATE above only
        // applies to fresh DBs). Adding a duplicate column errors — ignore that one case so the
        // migration is idempotent; any other error propagates.
        if let Err(e) = self.conn.execute(
            "ALTER TABLE events ADD COLUMN inbound INTEGER NOT NULL DEFAULT 0",
            [],
        ) {
            let msg = e.to_string();
            if !msg.contains("duplicate column name") {
                return Err(e);
            }
        }
        // Prune to the most recent MAX_ROWS / MAX_ALERTS.
        self.conn.execute(
            "DELETE FROM events WHERE id <= (SELECT COALESCE(MAX(id),0) FROM events) - ?1",
            params![MAX_ROWS],
        )?;
        self.conn.execute(
            "DELETE FROM alerts WHERE id <= (SELECT COALESCE(MAX(id),0) FROM alerts) - ?1",
            params![MAX_ALERTS],
        )?;
        Ok(())
    }

    /// Delete events + alerts older than `cutoff_ms` (time-based retention, #32). The row-count
    /// cap in init() still applies independently.
    pub fn prune_older_than(&self, cutoff_ms: i64) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM events WHERE at_ms < ?1", params![cutoff_ms])?;
        self.conn
            .execute("DELETE FROM alerts WHERE at_ms < ?1", params![cutoff_ms])?;
        Ok(())
    }

    /// Persist a batch of resolved feed rows (one transaction).
    pub fn insert(&mut self, rows: &[&FeedRow]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO events
                    (at_ms,process,host,dst_ip,port,protocol,pid,uid,src_ip,src_port,state,action,why,inbound)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            )?;
            for r in rows {
                let at_ms: i64 = r.at_ms.parse().unwrap_or(0);
                stmt.execute(params![
                    at_ms,
                    r.process,
                    r.host,
                    r.dst_ip,
                    r.port,
                    r.protocol,
                    r.pid,
                    r.uid,
                    r.src_ip,
                    r.src_port,
                    r.state,
                    r.action.clone().unwrap_or_default(),
                    r.why,
                    r.inbound as i64,
                ])?;
            }
        }
        tx.commit()
    }

    /// Most recent `limit` decisions, newest first.
    pub fn recent(&self, limit: u32) -> rusqlite::Result<Vec<FeedRow>> {
        self.window(0, limit)
    }

    /// Decisions at/after `since_ms` (Unix ms; 0 = all), newest first, capped at `limit`.
    /// Powers the time-window selector (live / last hour / today / last 7 days).
    pub fn window(&self, since_ms: i64, limit: u32) -> rusqlite::Result<Vec<FeedRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,at_ms,process,host,dst_ip,port,protocol,pid,uid,src_ip,src_port,state,action,why,inbound
             FROM events WHERE at_ms >= ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![since_ms, limit], |row| {
                let action: String = row.get(12)?;
                Ok(FeedRow {
                    id: row.get::<_, i64>(0)? as u64,
                    at_ms: row.get::<_, i64>(1)?.to_string(),
                    process: row.get(2)?,
                    host: row.get(3)?,
                    dst_ip: row.get(4)?,
                    port: row.get::<_, i64>(5)? as u32,
                    protocol: row.get(6)?,
                    pid: row.get::<_, i64>(7)? as u32,
                    uid: row.get::<_, i64>(8)? as u32,
                    src_ip: row.get(9)?,
                    src_port: row.get::<_, i64>(10)? as u32,
                    state: row.get(11)?,
                    action: if action.is_empty() {
                        None
                    } else {
                        Some(action)
                    },
                    why: row.get(13)?,
                    inbound: row.get::<_, i64>(14)? != 0,
                    // Process tree/args/container aren't persisted; historical rows lack them.
                    tree: Vec::new(),
                    args: Vec::new(),
                    group: String::new(),
                    container: String::new(),
                    first_seen: false,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Per-app drill-down (FR-006): this binary's destinations over time, aggregated by
    /// host:port (count + most-recent state/action/time), newest activity first.
    /// Inventory of every app that has reached the network (for the Apps view). Activity side
    /// only; posture is computed in main.rs from the rule set.
    pub fn apps(&self) -> rusqlite::Result<Vec<crate::AppInventory>> {
        let mut stmt = self.conn.prepare(
            "SELECT process,
                    COUNT(*) AS conns,
                    COUNT(DISTINCT host) AS hosts,
                    COALESCE(SUM(CASE WHEN state='allowed' THEN 1 ELSE 0 END),0) AS allowed,
                    COALESCE(SUM(CASE WHEN state='blocked' THEN 1 ELSE 0 END),0) AS blocked,
                    MAX(at_ms) AS last_ms
             FROM events WHERE process <> '' GROUP BY process ORDER BY last_ms DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(crate::AppInventory {
                    app: r.get::<_, String>(0)?,
                    conns: r.get::<_, i64>(1)? as u32,
                    hosts: r.get::<_, i64>(2)? as u32,
                    allowed: r.get::<_, i64>(3)? as u32,
                    blocked: r.get::<_, i64>(4)? as u32,
                    last_ms: r.get::<_, i64>(5)?.to_string(),
                    posture: String::new(),
                    rules: 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Inventory of every destination reached (for the Destinations view, #11). Keyed by hostname
    /// when known, else the IP; carries the distinct IPs seen so main.rs can compute posture and
    /// block by IP. Activity side only; posture is filled in main.rs from the engine rule set.
    pub fn dests(&self) -> rusqlite::Result<Vec<crate::DestInventory>> {
        let mut stmt = self.conn.prepare(
            "SELECT CASE WHEN host <> '' THEN host ELSE dst_ip END AS dest,
                    GROUP_CONCAT(DISTINCT dst_ip) AS ips,
                    COUNT(*) AS conns,
                    COUNT(DISTINCT process) AS apps,
                    COALESCE(SUM(CASE WHEN state='allowed' THEN 1 ELSE 0 END),0) AS allowed,
                    COALESCE(SUM(CASE WHEN state='blocked' THEN 1 ELSE 0 END),0) AS blocked,
                    MAX(at_ms) AS last_ms
             FROM events WHERE (host <> '' OR dst_ip <> '')
             GROUP BY dest ORDER BY last_ms DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let ips: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
                Ok(crate::DestInventory {
                    dest: r.get::<_, String>(0)?,
                    ips: ips
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect(),
                    conns: r.get::<_, i64>(2)? as u32,
                    apps: r.get::<_, i64>(3)? as u32,
                    allowed: r.get::<_, i64>(4)? as u32,
                    blocked: r.get::<_, i64>(5)? as u32,
                    last_ms: r.get::<_, i64>(6)?.to_string(),
                    posture: String::new(),
                    rules: 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn app_summary(&self, process: &str, scan: u32) -> rusqlite::Result<Vec<crate::AppHost>> {
        let mut stmt = self.conn.prepare(
            "SELECT at_ms,host,dst_ip,port,state,action FROM events
             WHERE process = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let iter = stmt.query_map(rusqlite::params![process, scan], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? as u32,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;

        use std::collections::HashMap;
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, crate::AppHost> = HashMap::new();
        for row in iter {
            let (at_ms, host, dst_ip, port, state, action) = row?;
            let key = format!("{host}|{port}|{state}");
            if let Some(h) = map.get_mut(&key) {
                h.count += 1; // rows are newest-first, so the first seen holds the latest verdict
            } else {
                order.push(key.clone());
                map.insert(
                    key,
                    crate::AppHost {
                        host,
                        dst_ip,
                        port,
                        count: 1,
                        state,
                        action: if action.is_empty() {
                            None
                        } else {
                            Some(action)
                        },
                        at_ms: at_ms.to_string(),
                    },
                );
            }
        }
        Ok(order.into_iter().filter_map(|k| map.remove(&k)).collect())
    }

    /// The apps that reached a given host, aggregated over history (usage by-host drill-down,
    /// FR-061). Mirrors `app_summary` but groups by process for a fixed host.
    pub fn host_summary(&self, host: &str, scan: u32) -> rusqlite::Result<Vec<crate::HostApp>> {
        let mut stmt = self.conn.prepare(
            "SELECT at_ms,process,dst_ip,port,state,action FROM events
             WHERE host = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let iter = stmt.query_map(rusqlite::params![host, scan], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? as u32,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;

        use std::collections::HashMap;
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, crate::HostApp> = HashMap::new();
        for row in iter {
            let (at_ms, process, dst_ip, port, state, action) = row?;
            let key = format!("{process}|{port}|{state}");
            if let Some(h) = map.get_mut(&key) {
                h.count += 1;
            } else {
                order.push(key.clone());
                map.insert(
                    key,
                    crate::HostApp {
                        process,
                        dst_ip,
                        port,
                        count: 1,
                        state,
                        action: if action.is_empty() {
                            None
                        } else {
                            Some(action)
                        },
                        at_ms: at_ms.to_string(),
                    },
                );
            }
        }
        Ok(order.into_iter().filter_map(|k| map.remove(&k)).collect())
    }

    /// The apps that reached a destination (#11 drill-down). Like `host_summary` but matches the
    /// same host-or-IP key the Destinations inventory groups by, so IP-only destinations (no
    /// resolved hostname) drill down correctly too.
    pub fn dest_summary(&self, dest: &str, scan: u32) -> rusqlite::Result<Vec<crate::HostApp>> {
        let mut stmt = self.conn.prepare(
            "SELECT at_ms,process,dst_ip,port,state,action FROM events
             WHERE (CASE WHEN host <> '' THEN host ELSE dst_ip END) = ?1
             ORDER BY id DESC LIMIT ?2",
        )?;
        let iter = stmt.query_map(rusqlite::params![dest, scan], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? as u32,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;

        use std::collections::HashMap;
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, crate::HostApp> = HashMap::new();
        for row in iter {
            let (at_ms, process, dst_ip, port, state, action) = row?;
            let key = format!("{process}|{port}|{state}");
            if let Some(h) = map.get_mut(&key) {
                h.count += 1;
            } else {
                order.push(key.clone());
                map.insert(
                    key,
                    crate::HostApp {
                        process,
                        dst_ip,
                        port,
                        count: 1,
                        state,
                        action: if action.is_empty() {
                            None
                        } else {
                            Some(action)
                        },
                        at_ms: at_ms.to_string(),
                    },
                );
            }
        }
        Ok(order.into_iter().filter_map(|k| map.remove(&k)).collect())
    }

    /// Inbound traffic that reached a given local port (#16: the Inbound-tab per-port drill-down).
    /// Aggregates the recorded INBOUND connections to `port` by remote peer + verdict, newest
    /// activity first, so you can see who's been hitting a port you've opened. Matches on port
    /// only — the engine stamps inbound rows as TCP, so protocol isn't a reliable filter here.
    pub fn port_summary(&self, port: u32, scan: u32) -> rusqlite::Result<Vec<crate::PortPeer>> {
        let mut stmt = self.conn.prepare(
            "SELECT at_ms,dst_ip,host,state,action FROM events
             WHERE inbound = 1 AND port = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let iter = stmt.query_map(rusqlite::params![port, scan], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;

        use std::collections::HashMap;
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, crate::PortPeer> = HashMap::new();
        for row in iter {
            let (at_ms, peer, host, state, action) = row?;
            let key = format!("{peer}|{state}");
            if let Some(p) = map.get_mut(&key) {
                p.count += 1; // newest-first, so the first seen holds the latest verdict
            } else {
                order.push(key.clone());
                map.insert(
                    key,
                    crate::PortPeer {
                        peer,
                        host,
                        count: 1,
                        state,
                        action: if action.is_empty() {
                            None
                        } else {
                            Some(action)
                        },
                        at_ms: at_ms.to_string(),
                    },
                );
            }
        }
        Ok(order.into_iter().filter_map(|k| map.remove(&k)).collect())
    }

    /// Top-talkers for the usage view (FR-061): rows aggregated by host (or by process when
    /// `by_host` is false) at/after `since_ms`, ordered by connection count. `count` is the
    /// number of recorded connection EVENTS — not bytes (the engine exposes connection counts, not byte volume).
    pub fn top_talkers(
        &self,
        by_host: bool,
        since_ms: i64,
        limit: u32,
    ) -> rusqlite::Result<Vec<crate::UsageRow>> {
        // `col` is a fixed identifier chosen by a bool, never user input — safe to interpolate.
        let col = if by_host { "host" } else { "process" };
        let sql = format!(
            "SELECT {col} AS name, COUNT(*) AS n,
                SUM(CASE WHEN state='allowed' THEN 1 ELSE 0 END) AS allowed,
                SUM(CASE WHEN state='blocked' THEN 1 ELSE 0 END) AS blocked,
                MAX(at_ms) AS last_ms
             FROM events WHERE at_ms >= ?1 AND {col} <> ''
             GROUP BY {col} ORDER BY n DESC, last_ms DESC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![since_ms, limit], |r| {
                Ok(crate::UsageRow {
                    name: r.get::<_, String>(0)?,
                    count: r.get::<_, i64>(1)? as u32,
                    allowed: r.get::<_, i64>(2)? as u32,
                    blocked: r.get::<_, i64>(3)? as u32,
                    last_ms: r.get::<_, i64>(4)?.to_string(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Append one security event to the log (FR-053). Uses a plain insert (alerts are low-volume).
    pub fn insert_alert(&self, a: &crate::AlertRow) -> rusqlite::Result<()> {
        let at_ms: i64 = a.at_ms.parse().unwrap_or(0);
        self.conn.execute(
            "INSERT INTO alerts (at_ms,level,priority,what,text,process,host)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![at_ms, a.level, a.priority, a.what, a.text, a.process, a.host],
        )?;
        Ok(())
    }

    /// Most recent `limit` security events, newest first (FR-053).
    pub fn recent_alerts(&self, limit: u32) -> rusqlite::Result<Vec<crate::AlertRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT at_ms,level,priority,what,text,process,host
             FROM alerts ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(crate::AlertRow {
                    at_ms: r.get::<_, i64>(0)?.to_string(),
                    level: r.get(1)?,
                    priority: r.get(2)?,
                    what: r.get(3)?,
                    text: r.get(4)?,
                    process: r.get(5)?,
                    host: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Delete all stored security events (FR-092 analogue). Does not touch the feed or rules.
    pub fn clear_alerts(&self) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM alerts", [])?;
        Ok(())
    }

    /// Distinct process paths ever recorded — used to seed the "new app" detector (FR-051).
    pub fn distinct_processes(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT process FROM events WHERE process <> ''")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Delete all stored history (FR-092). Does not touch engine rules.
    pub fn clear(&self) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM events", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FeedRow;

    /// A minimal FeedRow fixture; `inbound`/`port`/`dst_ip` are what the #16 drill-down keys on.
    fn row(at_ms: i64, port: u32, dst_ip: &str, state: &str, inbound: bool) -> FeedRow {
        FeedRow {
            id: 0,
            process: String::new(),
            host: String::new(),
            port,
            protocol: "tcp".into(),
            state: state.into(),
            action: Some(if state == "blocked" { "deny" } else { "allow" }.into()),
            why: String::new(),
            at_ms: at_ms.to_string(),
            pid: 0,
            uid: 0,
            src_ip: String::new(),
            src_port: 0,
            dst_ip: dst_ip.into(),
            tree: Vec::new(),
            args: Vec::new(),
            group: String::new(),
            container: String::new(),
            inbound,
            first_seen: false,
        }
    }

    #[test]
    fn inbound_flag_round_trips_and_port_summary_aggregates() {
        let mut h = History::in_memory();
        let rows = [
            row(100, 2222, "203.0.113.5", "allowed", true), // peer A → port 2222
            row(200, 2222, "203.0.113.5", "allowed", true), // peer A again (coalesces)
            row(300, 2222, "198.51.100.9", "blocked", true), // peer B → port 2222
            row(400, 443, "8.8.8.8", "allowed", false),     // outbound, different port
        ];
        h.insert(&rows.iter().collect::<Vec<_>>()).unwrap();

        // window() reads the inbound flag back (was hardcoded false before #16).
        let win = h.window(0, 100).unwrap();
        assert_eq!(win.len(), 4);
        assert_eq!(win.iter().filter(|r| r.inbound).count(), 3);

        // port_summary groups the inbound rows on 2222 by peer, newest-first, with counts.
        let peers = h.port_summary(2222, 5000).unwrap();
        assert_eq!(peers.len(), 2, "two distinct peers on 2222");
        assert_eq!(peers[0].peer, "198.51.100.9"); // most recent activity first
        assert_eq!(peers[0].state, "blocked");
        let peer_a = peers.iter().find(|p| p.peer == "203.0.113.5").unwrap();
        assert_eq!(peer_a.count, 2);
        assert_eq!(peer_a.state, "allowed");

        // The outbound row on 443 and unrelated ports don't leak in.
        assert!(h.port_summary(443, 5000).unwrap().is_empty());
    }

    /// A FeedRow fixture with a process + host, for the Destinations aggregation (#11).
    fn outrow(
        at_ms: i64,
        process: &str,
        host: &str,
        dst_ip: &str,
        port: u32,
        state: &str,
    ) -> FeedRow {
        FeedRow {
            process: process.into(),
            host: host.into(),
            dst_ip: dst_ip.into(),
            port,
            ..row(at_ms, port, dst_ip, state, false)
        }
    }

    #[test]
    fn dests_group_by_host_or_ip_with_ips_and_drilldown() {
        let mut h = History::in_memory();
        let rows = [
            // dns.google reached by two apps over two IPs.
            outrow(
                100,
                "/usr/bin/curl",
                "dns.google",
                "8.8.8.8",
                443,
                "allowed",
            ),
            outrow(
                200,
                "/usr/bin/firefox",
                "dns.google",
                "8.8.4.4",
                443,
                "blocked",
            ),
            // An IP-only destination (no resolved hostname) — must still group + drill down.
            outrow(300, "/usr/bin/curl", "", "203.0.113.7", 443, "allowed"),
        ];
        h.insert(&rows.iter().collect::<Vec<_>>()).unwrap();

        let dests = h.dests().unwrap();
        assert_eq!(dests.len(), 2, "dns.google + the bare IP");

        let g = dests.iter().find(|d| d.dest == "dns.google").unwrap();
        assert_eq!(g.conns, 2);
        assert_eq!(g.apps, 2, "two distinct apps");
        assert_eq!(g.allowed, 1);
        assert_eq!(g.blocked, 1);
        assert_eq!(
            g.ips.len(),
            2,
            "both resolved IPs captured for posture/blocking"
        );
        assert!(g.ips.contains(&"8.8.8.8".to_string()));

        // The IP-only destination is keyed by its IP and drills down by the same key.
        let ip_dest = dests.iter().find(|d| d.dest == "203.0.113.7").unwrap();
        assert_eq!(ip_dest.ips, vec!["203.0.113.7".to_string()]);
        let apps = h.dest_summary("203.0.113.7", 5000).unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].process, "/usr/bin/curl");
    }
}
