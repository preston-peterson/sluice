//! Sluice — the Tauri desktop app.
//!
//! Sluice runs on the Sluice engine: the Rust backend connects to the engine's gRPC
//! connection-event stream over UDS, bridges live feed events to the webview (`feed-batch`),
//! and drives the engine's rule RPCs for the two-click allow/block flow. The frontend is a
//! dependency-free static page (no JS build step).

// Hide the console window on Windows release builds (no-op on Linux).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod geoip;
mod history;
mod netstat;

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use history::History;
use serde::{Deserialize, Serialize};
use sluice_types::{pb, verdict, Action, ConnState, FeedEvent, Scope};
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast::error::RecvError;

/// A feed row shipped to the webview. Mirrors [`FeedEvent`] with primitives only (so the
/// engine stays serde-free); `at_ms` is a string because JS numbers can't hold u128.
#[derive(Clone, Serialize)]
struct FeedRow {
    id: u64,
    process: String,
    host: String,
    port: u32,
    protocol: String,
    state: String,
    action: Option<String>,
    why: String,
    at_ms: String,
    // Detail fields (shown when a row is expanded).
    pid: u32,
    uid: u32,
    src_ip: String,
    src_port: u32,
    dst_ip: String,
    /// Process ancestry as "comm(pid)" entries (from the daemon's process_tree).
    tree: Vec<String>,
    /// Process command-line args (from the daemon) — lets a generic interpreter like
    /// `python3.11` be identified by the script/module it's actually running.
    args: Vec<String>,
    /// Computed project/launcher label for grouping (FR-020) — the outermost non-system
    /// ancestor, else the process basename (or the container, when known).
    group: String,
    /// Container the process runs in (e.g. "uptime-kuma"), empty if none. Shown as a pill.
    container: String,
    /// True for an incoming connection (remote → local) — rendered distinctly (E6.0).
    inbound: bool,
    /// True if this is the first network connection we've ever seen from this binary (FR-051).
    first_seen: bool,
}

/// Core system/session processes that shouldn't be treated as a "project" root.
const SYSTEM_ANCESTORS: &[&str] = &[
    "systemd",
    "init",
    "(sd-pam)",
    "gnome-shell",
    "gnome-session-binary",
    "gnome-session",
    "plasmashell",
    "sddm",
    "lightdm",
    "gdm",
    "gdm-session-worker",
    "dbus-daemon",
    "dbus-broker",
];

/// Pick a project label for a connection (FR-020, process-tree strategy / DEC-007): the
/// outermost non-system ancestor (heuristic: smallest pid > 1 that isn't a system process,
/// which tends to be the session's launcher/shell), falling back to the process basename.
/// Kept isolated so the heuristic is easy to tune once we've validated against real trees.
fn project_label(conn: &pb::Connection) -> String {
    let mut best: Option<(&str, u32)> = None;
    for e in &conn.process_tree {
        let (name, pid) = (e.key.as_str(), e.value);
        // Match the skip-list against the BASENAME: the daemon reports some ancestors by
        // full path (e.g. "/usr/lib/systemd/systemd" for the user session manager), which
        // would otherwise evade the short "systemd" entry and collapse every GUI app into
        // one bogus "/usr/lib/systemd/systemd" group.
        if pid <= 1 || SYSTEM_ANCESTORS.contains(&basename(name)) {
            continue;
        }
        if best.is_none_or(|(_, bp)| pid < bp) {
            best = Some((name, pid));
        }
    }
    best.map(|(n, _)| basename(n).to_string())
        .unwrap_or_else(|| basename(&conn.process_path).to_string())
}

impl FeedRow {
    fn from_event(ev: &FeedEvent) -> Self {
        let state = match ev.state {
            ConnState::Pending => "pending",
            ConnState::Allowed => "allowed",
            ConnState::Blocked => "blocked",
        };
        FeedRow {
            id: ev.id,
            process: ev.conn.process_path.clone(),
            host: verdict::host_of(&ev.conn),
            port: ev.conn.dst_port,
            protocol: ev.conn.protocol.clone(),
            state: state.to_string(),
            action: ev.action.map(|a| a.as_str().to_string()),
            why: ev.why.clone(),
            at_ms: ev.at_unix_ms.to_string(),
            pid: ev.conn.process_id,
            uid: ev.conn.user_id,
            src_ip: ev.conn.src_ip.clone(),
            src_port: ev.conn.src_port,
            dst_ip: ev.conn.dst_ip.clone(),
            tree: ev
                .conn
                .process_tree
                .iter()
                .map(|e| format!("{}({})", e.key, e.value))
                .collect(),
            args: ev.conn.process_args.clone(),
            // Group by container when known (so e.g. all of uptime-kuma's probes collapse under
            // one group), else fall back to the process-tree/basename heuristic.
            group: if ev.container.is_empty() {
                project_label(&ev.conn)
            } else {
                ev.container.clone()
            },
            container: ev.container.clone(),
            inbound: ev.inbound,
            first_seen: false, // set by the bridge when the process is seen for the first time
        }
    }
}

/// Map an engine `ConnEvent` (E3.0) into the UI's `FeedEvent` — an already-resolved allow/block.
/// Fields the engine doesn't yet provide (src, dst_host/SNI, process tree/args) are left empty;
/// the feed stays honest about what it shows. `process_path` falls back to `comm`.
fn engine_event_to_feed(ev: sluice_proto::ConnEvent, id: u64) -> FeedEvent {
    let block = ev.verdict == 1;
    let process_path = if !ev.process_path.is_empty() {
        ev.process_path
    } else {
        ev.comm
    };
    let conn = pb::Connection {
        // Real L4 protocol from the engine (#14); fall back to tcp for the rare empty/unknown
        // case so historical filtering stays sane.
        protocol: if ev.protocol.is_empty() {
            "tcp".to_string()
        } else {
            ev.protocol
        },
        dst_ip: ev.dst_ip,
        dst_host: ev.dst_host, // from the engine DNS snoop (E4); empty if unknown
        dst_port: ev.dst_port,
        user_id: ev.uid,
        process_id: ev.pid,
        process_path,
        process_args: ev.process_args, // /proc cmdline — "which node/python" (engine enrichment)
        ..Default::default()
    };
    FeedEvent {
        id,
        conn,
        state: if block {
            ConnState::Blocked
        } else {
            ConnState::Allowed
        },
        action: Some(if block { Action::Deny } else { Action::Allow }),
        scope: None,
        duration: None,
        why: if ev.inbound {
            if block {
                "inbound (blocked)".to_string()
            } else {
                "inbound (observed)".to_string()
            }
        } else if block {
            "blocked by rule".to_string()
        } else {
            "allowed (monitor)".to_string()
        },
        at_unix_ms: ev.at_unix_ms as u128,
        container: ev.container,
        inbound: ev.inbound,
    }
}

/// Engine link (E3.0/E3.2): connect to the Sluice engine's gRPC stream over UDS and republish
/// each observed connection into the UI's feed pipeline as a `FeedEvent`. Reconnects with
/// backoff so the UI tolerates the engine starting after it (or restarting), and tracks the
/// link state (`connected`) for the status pill, firing a desktop notification on transitions.
async fn run_engine_client(
    uds: String,
    tx: tokio::sync::broadcast::Sender<FeedEvent>,
    connected: Arc<AtomicBool>,
    app_handle: tauri::AppHandle,
) {
    use sluice_proto::{sluice_engine_client::SluiceEngineClient, WatchRequest};
    use tonic::transport::{Endpoint, Uri};

    let mut id: u64 = 0;
    loop {
        // Custom connector: the URI is a placeholder; we always dial the UDS path.
        let path = uds.clone();
        let connector = tower::service_fn(move |_: Uri| {
            let path = path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(&path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        });
        let channel = match Endpoint::try_from("http://127.0.0.1:50051")
            .expect("static placeholder endpoint")
            .connect_with_connector(connector)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "engine link: connect failed; retry in 2s");
                engine_link_down(&connected, &app_handle);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let mut client = SluiceEngineClient::new(channel);
        let mut stream = match client.watch_connections(WatchRequest {}).await {
            Ok(s) => s.into_inner(),
            Err(e) => {
                tracing::warn!(error = %e, "engine link: watch_connections failed; retry in 2s");
                engine_link_down(&connected, &app_handle);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        // Connected — note the transition (false → true) once.
        if !connected.swap(true, Ordering::Relaxed) {
            tracing::info!("engine link: connected");
            let _ = app_handle
                .notification()
                .builder()
                .title("Sluice — engine connected")
                .body("Monitoring outbound connections.")
                .show();
        }
        loop {
            match stream.message().await {
                Ok(Some(ev)) => {
                    id = id.wrapping_add(1);
                    let _ = tx.send(engine_event_to_feed(ev, id));
                }
                Ok(None) => {
                    tracing::warn!("engine link: stream ended; reconnecting");
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "engine link: stream error; reconnecting");
                    break;
                }
            }
        }
        engine_link_down(&connected, &app_handle);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Mark the engine link down; on the true → false transition, notify the user.
fn engine_link_down(connected: &Arc<AtomicBool>, app_handle: &tauri::AppHandle) {
    if connected.swap(false, Ordering::Relaxed) {
        tracing::warn!("engine link: disconnected");
        let _ = app_handle
            .notification()
            .builder()
            .title("Sluice — engine offline")
            .body("New connections aren't being monitored.")
            .show();
    }
}

// ----- engine decision RPCs (E3.1) -----------------------------------------------------
//
// The Tauri rule commands are synchronous and run off the tokio runtime, so each does a quick
// block_on a fresh current-thread runtime to make the (rare, user-initiated) gRPC call. A
// per-call UDS connect is cheap and keeps these stateless.

/// Connect a SluiceEngine gRPC client over the engine's UDS.
async fn engine_connect(
    uds: &str,
) -> anyhow::Result<sluice_proto::sluice_engine_client::SluiceEngineClient<tonic::transport::Channel>>
{
    use tonic::transport::{Endpoint, Uri};
    let path = uds.to_string();
    let connector = tower::service_fn(move |_: Uri| {
        let path = path.clone();
        async move {
            let stream = tokio::net::UnixStream::connect(&path).await?;
            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        }
    });
    let channel = Endpoint::try_from("http://127.0.0.1:50051")?
        .connect_with_connector(connector)
        .await?;
    Ok(sluice_proto::sluice_engine_client::SluiceEngineClient::new(
        channel,
    ))
}

/// Run a short-lived gRPC call on a throwaway current-thread runtime (we're off the main rt).
fn engine_block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime")
        .block_on(f)
}

/// `SetRule(block, ip, port)` — port 0 = any port. Returns the engine's error on refusal.
fn engine_set_rule(uds: &str, ip: &str, port: u32) -> Result<(), String> {
    engine_block_on(async {
        let mut c = engine_connect(uds).await.map_err(|e| e.to_string())?;
        let ack = c
            .set_rule(sluice_proto::Rule {
                id: String::new(),
                action: 1,
                dst_ip: ip.to_string(),
                dst_port: port,
            })
            .await
            .map_err(|e| e.to_string())?
            .into_inner();
        if ack.ok {
            Ok(())
        } else {
            Err(ack.error)
        }
    })
}

/// `RemoveRule(id)`. Returns the engine's error (e.g. "no such rule").
fn engine_remove_rule(uds: &str, id: &str) -> Result<(), String> {
    engine_block_on(async {
        let mut c = engine_connect(uds).await.map_err(|e| e.to_string())?;
        let ack = c
            .remove_rule(sluice_proto::RuleId { id: id.to_string() })
            .await
            .map_err(|e| e.to_string())?
            .into_inner();
        if ack.ok {
            Ok(())
        } else {
            Err(ack.error)
        }
    })
}

/// `ListRules` → rows for the rules panel.
fn engine_list_rules(uds: &str) -> Result<Vec<RuleDto>, String> {
    engine_block_on(async {
        let mut c = engine_connect(uds).await.map_err(|e| e.to_string())?;
        let list = c
            .list_rules(sluice_proto::ListRequest {})
            .await
            .map_err(|e| e.to_string())?
            .into_inner();
        Ok(list
            .rules
            .into_iter()
            .map(|r| RuleDto {
                name: r.id,
                action: "deny".to_string(),
                app: String::new(),
                host: r.dst_ip.clone(),
                detail: if r.dst_port == 0 {
                    format!("dest.ip={} (any port)", r.dst_ip)
                } else {
                    format!("dest.ip={} port={}", r.dst_ip, r.dst_port)
                },
                source: "engine".to_string(),
                system: false,
                removable: true,
            })
            .collect())
    })
}

/// Map a UI Block/Allow decision onto the engine's IP+port rule model (E3.1). App-scoped rules
/// and durations aren't supported by the engine yet (the UI hides them in engine mode).
fn engine_create_rule(
    uds: &str,
    action: Action,
    scope: Scope,
    host: &str,
    port: u32,
) -> RuleResult {
    let ip = host.trim().to_string();
    if matches!(scope, Scope::AppToAny | Scope::AppToHost) {
        return RuleResult {
            ok: false,
            message: "app-scoped rules aren't supported by the Sluice engine yet — block by host or this connection".to_string(),
        };
    }
    if ip.is_empty() {
        return RuleResult {
            ok: false,
            message: "no destination IP".to_string(),
        };
    }
    if ip.parse::<std::net::IpAddr>().is_err() {
        return RuleResult {
            ok: false,
            message: format!("engine rules need an IP address (got '{ip}')"),
        };
    }
    // HostToAny → any port (0); Connection → the specific port.
    let eport = if matches!(scope, Scope::HostToAny) {
        0
    } else {
        port
    };

    if action.is_allow() {
        // Allow under default-allow = remove a matching block (no-op if there isn't one).
        let id = format!("v4:{ip}:{eport}");
        match engine_remove_rule(uds, &id) {
            Ok(()) => RuleResult {
                ok: true,
                message: format!("unblocked {ip}"),
            },
            Err(e) if e.contains("no such rule") => RuleResult {
                ok: true,
                message: format!("{ip} already allowed"),
            },
            Err(e) => RuleResult {
                ok: false,
                message: e,
            },
        }
    } else {
        match engine_set_rule(uds, &ip, eport) {
            Ok(()) => RuleResult {
                ok: true,
                message: format!("blocked {ip}"),
            },
            Err(e) => RuleResult {
                ok: false,
                message: e,
            },
        }
    }
}

// ----- inbound policy (E6.1) -----------------------------------------------------------

/// An allowed inbound port, shipped to/from the webview.
#[derive(Serialize, Deserialize, Clone)]
struct PortDto {
    proto: String, // "tcp" | "udp"
    port: u32,
}

/// The inbound posture for the webview.
#[derive(Serialize)]
struct InboundDto {
    enforce: bool,
    allow: Vec<PortDto>,
}

fn engine_get_inbound(uds: &str) -> Result<InboundDto, String> {
    engine_block_on(async {
        let mut c = engine_connect(uds).await.map_err(|e| e.to_string())?;
        let p = c
            .get_inbound_policy(sluice_proto::InboundQuery {})
            .await
            .map_err(|e| e.to_string())?
            .into_inner();
        Ok(InboundDto {
            enforce: p.enforce,
            allow: p
                .allow
                .into_iter()
                .map(|a| PortDto {
                    proto: a.proto,
                    port: a.port,
                })
                .collect(),
        })
    })
}

fn engine_set_inbound(uds: &str, enforce: bool, allow: Vec<PortDto>) -> Result<(), String> {
    engine_block_on(async {
        let mut c = engine_connect(uds).await.map_err(|e| e.to_string())?;
        let ack = c
            .set_inbound_policy(sluice_proto::InboundPolicy {
                enforce,
                allow: allow
                    .into_iter()
                    .map(|p| sluice_proto::InboundAllow {
                        proto: p.proto,
                        port: p.port,
                    })
                    .collect(),
            })
            .await
            .map_err(|e| e.to_string())?
            .into_inner();
        if ack.ok {
            Ok(())
        } else {
            Err(ack.error)
        }
    })
}

/// Rule entry shipped to the rules panel (built from the engine's `ListRules`).
#[derive(Serialize)]
struct RuleDto {
    name: String,
    action: String,
    app: String,
    host: String,
    /// Compact operator summary (operand=data, …) for rules that aren't a simple app/host.
    detail: String,
    /// "engine" — the rule's source.
    source: String,
    /// System defaults (e.g. localhost allows) — shown but protected from removal.
    system: bool,
    removable: bool,
}

struct AppState {
    /// Local SQLite history store (DEC-005) — Sluice-owned, separate from engine rules.
    history: Arc<Mutex<History>>,
    /// History retention in days (#32); 0 = keep forever. Persisted to data_dir/retention.
    retention_days: Arc<Mutex<i64>>,
    /// Rule commands target the Sluice engine over this UDS (the socket path).
    engine_uds: String,
    /// True while the engine's connection-event stream is up (drives the status pill, E3.2).
    engine_connected: Arc<AtomicBool>,
}

/// Sluice's local data directory (XDG data home, else ~/.local/share/sluice).
fn data_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("sluice");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("sluice")
}

/// Create the Sluice data dir if needed and lock it to the owner (0700). It holds the connection
/// history DB and profiles (PII), so it must not be world-readable/traversable (SEC-009).
fn ensure_data_dir() -> PathBuf {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir
}

fn retention_path() -> PathBuf {
    data_dir().join("retention")
}

/// Read the persisted history-retention setting (days; 0 = keep forever). Default 30.
fn load_retention() -> i64 {
    std::fs::read_to_string(retention_path())
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|d| *d >= 0)
        .unwrap_or(30)
}

fn save_retention(days: i64) {
    let _ = ensure_data_dir();
    let p = retention_path();
    if std::fs::write(&p, days.to_string()).is_ok() {
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
}

/// Delete history older than `days` (0 = no time-based pruning; the row-count cap still applies).
fn prune_history(history: &Arc<Mutex<History>>, days: i64) {
    if days <= 0 {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let cutoff = now - days * 86_400_000;
    if let Ok(h) = history.lock() {
        let _ = h.prune_older_than(cutoff);
    }
}

/// Most recent decisions from the local history store (newest first), to seed the feed on
/// startup so it survives restarts.
#[tauri::command]
fn history_recent(state: State<AppState>, limit: Option<u32>) -> Vec<FeedRow> {
    let n = limit.unwrap_or(500).min(5000);
    state
        .history
        .lock()
        .ok()
        .and_then(|h| h.recent(n).ok())
        .unwrap_or_default()
}

/// One destination of an app, aggregated over history (for the per-app drill-down, FR-006).
#[derive(Serialize)]
struct AppHost {
    host: String,
    dst_ip: String,
    port: u32,
    count: u32,
    state: String,
    action: Option<String>,
    at_ms: String,
}

/// One app that reached a given host, aggregated over history (usage by-host drill-down, FR-061).
#[derive(Serialize)]
struct HostApp {
    process: String,
    dst_ip: String,
    port: u32,
    count: u32,
    state: String,
    action: Option<String>,
    at_ms: String,
}

/// One remote peer that connected to a given local port, aggregated over history (#16: the
/// Inbound-tab per-port drill-down). `peer` is the remote IP, `host` its resolved name (if known).
#[derive(Serialize)]
struct PortPeer {
    peer: String,
    host: String,
    count: u32,
    state: String,
    action: Option<String>,
    at_ms: String,
}

/// A top-talker row for the usage view (FR-061): one host or app aggregated over a window.
/// `count` is CONNECTION EVENTS, not bytes — the engine exposes connection counts, not byte volume.
#[derive(Serialize)]
struct UsageRow {
    name: String,
    count: u32,
    allowed: u32,
    blocked: u32,
    last_ms: String,
}

/// One app in the Apps permissions view (control panel): activity from history plus the posture
/// computed from the current rule set.
#[derive(Serialize)]
struct AppInventory {
    app: String, // process path
    conns: u32,
    hosts: u32,
    allowed: u32,
    blocked: u32,
    last_ms: String,
    posture: String, // allowed | blocked | mixed | monitoring
    rules: u32,
}

/// One destination in the Destinations permissions view (#11): activity from history plus the
/// posture computed from the engine's (dest-keyed) rule set. Unlike apps, this maps cleanly onto
/// the engine — a destination is "blocked" when a deny rule covers one of its IPs.
#[derive(Serialize)]
struct DestInventory {
    dest: String,     // hostname if known, else the IP
    ips: Vec<String>, // distinct IPs this dest resolved to (for posture + IP-keyed blocking)
    conns: u32,
    apps: u32,
    allowed: u32,
    blocked: u32,
    last_ms: String,
    posture: String, // blocked | monitoring (engine is default-allow + denylist)
    rules: u32,      // engine deny rules touching this dest's IPs
}

/// Usage / top-talkers (FR-061): the most-active hosts or apps by connection count for a chosen
/// window, from Sluice's own history. `by` = "host" (default) or "app". Counts are connections,
/// not bytes. `since` is Unix ms (0 = all time).
#[tauri::command]
fn usage(state: State<AppState>, by: String, since: i64, limit: Option<u32>) -> Vec<UsageRow> {
    let by_host = by != "app";
    let limit = limit.unwrap_or(50);
    state
        .history
        .lock()
        .ok()
        .and_then(|h| h.top_talkers(by_host, since, limit).ok())
        .unwrap_or_default()
}

/// Feed-history retention (#32): days to keep, 0 = forever. Backend-owned + persisted; pruned on
/// startup, on a timer, and immediately when changed.
#[tauri::command]
fn get_retention(state: State<AppState>) -> i64 {
    state.retention_days.lock().map(|g| *g).unwrap_or(30)
}

#[tauri::command]
fn set_retention(state: State<AppState>, days: i64) {
    let d = days.max(0);
    if let Ok(mut g) = state.retention_days.lock() {
        *g = d;
    }
    save_retention(d);
    prune_history(&state.history, d);
}

/// A security event (FR-053) shipped to the UI and persisted. `at_ms` is a string (JS u128 safe).
#[derive(Clone, Serialize)]
struct AlertRow {
    at_ms: String,
    level: String,    // info | warning | error
    priority: String, // low | medium | high
    what: String, // process | firewall | connection | rule | netlink | kernel | generic | new-app
    text: String,
    process: String,
    host: String,
}

/// A throughput sample (FR-060): bytes/sec in and out across non-loopback interfaces.
#[derive(Clone, Serialize)]
struct Throughput {
    rx_bps: u64,
    tx_bps: u64,
    at_ms: String,
}

/// Persist a security event and push it live to the webview (FR-053).
fn record_alert(app: &tauri::AppHandle, history: &Arc<Mutex<History>>, row: AlertRow) {
    if let Ok(h) = history.lock() {
        if let Err(e) = h.insert_alert(&row) {
            tracing::warn!(error = %e, "alert insert failed");
        }
    }
    let _ = app.emit("security-event", &row);
}

/// Recent security events, newest first (FR-053).
#[tauri::command]
fn security_events(state: State<AppState>, limit: Option<u32>) -> Vec<AlertRow> {
    let limit = limit.unwrap_or(300);
    state
        .history
        .lock()
        .ok()
        .and_then(|h| h.recent_alerts(limit).ok())
        .unwrap_or_default()
}

/// Clear the security event log (does not touch the feed or firewall rules).
#[tauri::command]
fn security_clear(state: State<AppState>) -> bool {
    state
        .history
        .lock()
        .map(|h| h.clear_alerts().is_ok())
        .unwrap_or(false)
}

/// Offline IP→country lookup (FR-052): ISO alpha-2 code, or None if no DB / not found.
#[tauri::command]
fn geo_country(ip: String) -> Option<String> {
    geoip::country_code(&ip)
}

/// On-demand reverse DNS (FR-052). User-initiated — a DNS query is sent only when the user clicks
/// "resolve", via the system resolver (`getent` → NSS/DNS), so the default feed stays quiet
/// (SEC-007). Returns the PTR hostname, or None (not an IP / no record / resolver missing).
#[tauri::command]
fn rdns(ip: String) -> Option<String> {
    // Only ever hand `getent` a validated IP literal (no arbitrary args).
    if ip.parse::<std::net::IpAddr>().is_err() {
        return None;
    }
    let out = std::process::Command::new("getent")
        .arg("hosts")
        .arg(&ip)
        .output()
        .ok()?;
    // "8.8.8.8   dns.google" → the first hostname is the 2nd whitespace field.
    let text = String::from_utf8_lossy(&out.stdout);
    let name = text.lines().next()?.split_whitespace().nth(1)?.to_string();
    (!name.is_empty() && name != ip).then_some(name)
}

/// Open an external URL in the user's browser — the FR-052 "investigate host" link. Only http(s)
/// (passed as a single arg to xdg-open, so no shell injection). User-initiated, so a destination
/// is disclosed to the external service only on an explicit click (SEC-007 stays intact for the
/// default, quiet feed).
#[tauri::command]
fn open_url(url: String) -> bool {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        tracing::warn!(%url, "refused to open non-http(s) url");
        return false;
    }
    std::process::Command::new("xdg-open")
        .arg(&url)
        .spawn()
        .map(|_| true)
        .unwrap_or(false)
}

/// The version Sluice was built at (the repo `VERSION` file, embedded at compile time).
const VERSION: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../VERSION"));

/// The built version, for display in the header (no network — just the embedded string).
#[tauri::command]
fn app_version() -> String {
    VERSION.trim().to_string()
}

/// Result of an update check, shipped to the webview.
#[derive(Serialize)]
struct UpdateInfo {
    current: String,
    latest: String,
    update_available: bool,
    url: String,
    error: String,
}

/// Compare dotted numeric versions; true if `latest` is strictly newer than `current`.
fn version_newer(latest: &str, current: &str) -> bool {
    let parts = |s: &str| -> Vec<u32> {
        s.trim()
            .trim_start_matches('v')
            .split('.')
            .map(|x| x.trim().parse().unwrap_or(0))
            .collect()
    };
    let (l, c) = (parts(latest), parts(current));
    for i in 0..l.len().max(c.len()) {
        let (a, b) = (
            l.get(i).copied().unwrap_or(0),
            c.get(i).copied().unwrap_or(0),
        );
        if a != b {
            return a > b;
        }
    }
    false
}

/// Check GitHub Releases for a newer version (update *alert* only — no download).
///
/// SEC-007: this is the ONE network call Sluice can make, and only when the user invokes it
/// (a manual "Check for updates" click, or the opt-in auto-check toggle which the webview gates).
/// It shells out to `curl` rather than embedding an HTTP client, so the request is explicit and
/// even shows up in Sluice's own feed. Fails closed (returns an error string, never panics).
#[tauri::command]
fn check_for_update() -> UpdateInfo {
    let current = VERSION.trim().to_string();
    let mut info = UpdateInfo {
        current: current.clone(),
        latest: String::new(),
        update_available: false,
        url: String::new(),
        error: String::new(),
    };
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "8",
            "-H",
            "Accept: application/vnd.github+json",
            "https://api.github.com/repos/preston-peterson/sluice/releases/latest",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => match serde_json::from_slice::<serde_json::Value>(&o.stdout)
        {
            Ok(v) => {
                let latest = v
                    .get("tag_name")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .trim_start_matches('v')
                    .to_string();
                info.url = v
                    .get("html_url")
                    .and_then(|u| u.as_str())
                    .unwrap_or("")
                    .to_string();
                info.update_available = !latest.is_empty() && version_newer(&latest, &current);
                info.latest = latest;
            }
            Err(_) => info.error = "no release published yet".to_string(),
        },
        Ok(_) => info.error = "no release published yet".to_string(),
        Err(e) => info.error = format!("check failed (is curl installed?): {e}"),
    }
    info
}

/// Pick the `.deb` asset (name + URL) and its `.sha256` sibling URL from a release JSON.
fn select_deb_assets(v: &serde_json::Value) -> Option<(String, String, String)> {
    let assets = v.get("assets")?.as_array()?;
    let mut deb: Option<(String, String)> = None;
    let mut sha: Option<String> = None;
    for a in assets {
        let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let url = a
            .get("browser_download_url")
            .and_then(|u| u.as_str())
            .unwrap_or("");
        if name.is_empty() || url.is_empty() {
            continue;
        }
        if name.ends_with(".deb.sha256") {
            sha = Some(url.to_string());
        } else if name.ends_with(".deb") {
            deb = Some((name.to_string(), url.to_string()));
        }
    }
    let (deb_name, deb_url) = deb?;
    Some((deb_name, deb_url, sha?))
}

/// Find the detached minisign signature asset (`<deb>.minisig`) URL in a release JSON.
fn select_sig_asset(v: &serde_json::Value, deb_name: &str) -> Option<String> {
    let want = format!("{deb_name}.minisig");
    v.get("assets")?.as_array()?.iter().find_map(|a| {
        if a.get("name").and_then(|n| n.as_str()) == Some(want.as_str()) {
            a.get("browser_download_url")
                .and_then(|u| u.as_str())
                .map(String::from)
        } else {
            None
        }
    })
}

/// The Ed25519 release-signing public key (minisign format). Releases are signed with the matching
/// secret key, held offline; the updater verifies every downloaded package against this BEFORE
/// installing — so a tampered `.deb` (even one served with a matching `.sha256`) is rejected.
const RELEASE_PUBKEY_FILE: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/sluice-release.pub"));

/// The base64 key line from the embedded minisign public-key file (skips the comment line).
fn release_pubkey_b64() -> &'static str {
    RELEASE_PUBKEY_FILE
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment"))
        .unwrap_or("")
}

/// Verify a downloaded package against its detached minisign signature with the embedded public
/// key — a pure in-process Ed25519 check (no external `minisign` needed). Err on any problem.
fn verify_signature(file: &std::path::Path, sig_text: &str) -> Result<(), String> {
    use minisign_verify::{PublicKey, Signature};
    let pk = PublicKey::from_base64(release_pubkey_b64())
        .map_err(|e| format!("bad embedded public key: {e}"))?;
    let sig = Signature::decode(sig_text).map_err(|e| format!("malformed signature: {e}"))?;
    let data = std::fs::read(file).map_err(|e| format!("couldn't read the package: {e}"))?;
    pk.verify(&data, &sig, false)
        .map_err(|e| format!("signature does not verify: {e}"))
}

/// Outcome of an in-app update apply, shipped to the webview.
#[derive(Serialize)]
struct UpdateApplyResult {
    ok: bool,
    /// Where it ended: "done" on success, otherwise the failing stage.
    stage: String,
    message: String,
    version: String,
}

/// Download the latest release's `.deb`, verify its signature + SHA-256, and install it via polkit.
///
/// SEC-001: the privileged install goes through `pkexec` (polkit) — the app never runs as root.
/// SEC-007: runs only on an explicit user click; same single `curl` network path as the check.
/// Authenticity is checked FIRST: the `.deb`'s Ed25519 (minisign) signature is verified against the
/// embedded public key, and the install is refused on a missing/invalid signature — so a tampered
/// package is rejected even if its `.sha256` matches. The SHA-256 is then checked for integrity, all
/// before install. Emits `update-progress` (stage strings); never panics (returns a structured result).
#[tauri::command]
fn download_and_apply_update(app: tauri::AppHandle) -> UpdateApplyResult {
    let fail = |stage: &str, msg: String| UpdateApplyResult {
        ok: false,
        stage: stage.to_string(),
        message: msg,
        version: String::new(),
    };
    let step = |stage: &str| {
        let _ = app.emit("update-progress", stage);
    };

    // 1. Resolve the .deb + .sha256 assets from the latest release.
    step("checking");
    let body = match std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "-H",
            "Accept: application/vnd.github+json",
            "https://api.github.com/repos/preston-peterson/sluice/releases/latest",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return fail("checking", "couldn't reach GitHub Releases.".into()),
    };
    let json: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return fail("checking", "couldn't read the release data.".into()),
    };
    let version = json
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    let (deb_name, deb_url, sha_url) = match select_deb_assets(&json) {
        Some(t) => t,
        None => {
            return fail(
                "checking",
                "this release has no installable .deb asset.".into(),
            )
        }
    };
    let sig_url = match select_sig_asset(&json, &deb_name) {
        Some(u) => u,
        None => {
            return fail(
                "checking",
                "this release isn't signed (no .minisig) — refusing to auto-install; use the manual download.".into(),
            )
        }
    };

    // 2. Private temp dir (0700).
    let dir = std::env::temp_dir().join(format!("sluice-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return fail("download", format!("couldn't create a temp dir: {e}"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let deb_path = dir.join(&deb_name);
    let sha_path = dir.join(format!("{deb_name}.sha256"));
    let sig_path = dir.join(format!("{deb_name}.minisig"));
    let cleanup = |dir: &std::path::Path| {
        let _ = std::fs::remove_dir_all(dir);
    };

    // 3. Download the package + its checksum + its signature.
    step("downloading");
    let dl = |url: &str, to: &std::path::Path| -> bool {
        std::process::Command::new("curl")
            .args(["-fsSL", "--max-time", "180", "-o"])
            .arg(to)
            .arg(url)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if !dl(&deb_url, &deb_path) {
        cleanup(&dir);
        return fail("download", "failed to download the update package.".into());
    }
    if !dl(&sha_url, &sha_path) {
        cleanup(&dir);
        return fail("download", "failed to download the checksum.".into());
    }
    if !dl(&sig_url, &sig_path) {
        cleanup(&dir);
        return fail("download", "failed to download the signature.".into());
    }

    // 4a. Verify the Ed25519 signature (authenticity) — the strong check, before checksum/install.
    step("verifying-sig");
    let sig_text = match std::fs::read_to_string(&sig_path) {
        Ok(s) => s,
        Err(e) => {
            cleanup(&dir);
            return fail("verifying-sig", format!("couldn't read the signature: {e}"));
        }
    };
    if let Err(e) = verify_signature(&deb_path, &sig_text) {
        cleanup(&dir);
        return fail(
            "verifying-sig",
            format!(
                "signature check FAILED — update aborted ({e}). The download may be tampered with."
            ),
        );
    }

    // 4b. Verify SHA-256 (integrity; abort hard on mismatch).
    step("verifying");
    let verified = std::process::Command::new("sha256sum")
        .arg("-c")
        .arg(&sha_path)
        .current_dir(&dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !verified {
        cleanup(&dir);
        return fail(
            "verifying",
            "checksum verification FAILED — update aborted (the download may be corrupt or tampered with).".into(),
        );
    }

    // 5. Install via polkit (prompts for the user's password).
    step("installing");
    let status = std::process::Command::new("pkexec")
        .arg("apt-get")
        .arg("install")
        .arg("-y")
        .arg(&deb_path)
        .status();
    cleanup(&dir);
    match status {
        Ok(s) if s.success() => UpdateApplyResult {
            ok: true,
            stage: "done".into(),
            message: format!("Sluice {version} installed."),
            version,
        },
        Ok(s) => {
            let code = s.code().unwrap_or(-1);
            // pkexec: 126 = not authorized / dialog dismissed, 127 = auth could not be obtained.
            let msg = if code == 126 || code == 127 {
                "installation was cancelled (authorization not granted).".to_string()
            } else {
                format!(
                    "the install step failed (exit {code}). You can download it manually instead."
                )
            };
            fail("installing", msg)
        }
        Err(e) => fail(
            "installing",
            format!("couldn't launch the installer (pkexec): {e}"),
        ),
    }
}

/// Relaunch the app — used after an update is installed, so the new binary takes over.
#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

/// An app's destinations over time (FR-006), newest activity first.
#[tauri::command]
fn app_history(state: State<AppState>, app: String) -> Vec<AppHost> {
    state
        .history
        .lock()
        .ok()
        .and_then(|h| h.app_summary(&app, 5000).ok())
        .unwrap_or_default()
}

/// The apps that reached a host over time (usage by-host drill-down, FR-061), newest first.
#[tauri::command]
fn host_history(state: State<AppState>, host: String) -> Vec<HostApp> {
    state
        .history
        .lock()
        .ok()
        .and_then(|h| h.host_summary(&host, 5000).ok())
        .unwrap_or_default()
}

/// The apps that reached a destination over time (#11 drill-down), newest first. Matches the
/// host-or-IP key the Destinations inventory uses, so IP-only destinations work too.
#[tauri::command]
fn dest_history(state: State<AppState>, dest: String) -> Vec<HostApp> {
    state
        .history
        .lock()
        .ok()
        .and_then(|h| h.dest_summary(&dest, 5000).ok())
        .unwrap_or_default()
}

/// The remote peers that connected to a given local port (#16: Inbound per-port drill-down),
/// newest activity first. Lets you see the traffic an opened inbound port has actually carried.
#[tauri::command]
fn port_history(state: State<AppState>, port: u32) -> Vec<PortPeer> {
    state
        .history
        .lock()
        .ok()
        .and_then(|h| h.port_summary(port, 5000).ok())
        .unwrap_or_default()
}

/// Short, curated descriptions for common noisy daemons/tools (clearer than man/pkg text).
const CURATED_PROCESSES: &[(&str, &str)] = &[
    (
        "systemd-resolved",
        "systemd's DNS resolver — most apps' name lookups go through it.",
    ),
    (
        "systemd-timesyncd",
        "systemd's network time (NTP) client — keeps the clock synced.",
    ),
    (
        "chronyd",
        "Chrony NTP daemon — keeps the system clock synced over the network.",
    ),
    (
        "NetworkManager",
        "Manages network connections (Wi-Fi / Ethernet / VPN).",
    ),
    (
        "tailscaled",
        "Tailscale daemon — WireGuard-based mesh VPN networking.",
    ),
    (
        "avahi-daemon",
        "Avahi (mDNS/zeroconf) — discovers services and .local names on the LAN.",
    ),
    (
        "dbus-daemon",
        "D-Bus message bus — local IPC between desktop apps and services.",
    ),
    (
        "dbus-broker",
        "D-Bus message bus — local IPC between desktop apps and services.",
    ),
    (
        "snapd",
        "Snap package daemon — manages snap apps and their updates.",
    ),
    (
        "packagekitd",
        "PackageKit — background software install/update service.",
    ),
    ("fwupd", "Firmware update daemon."),
    ("cupsd", "CUPS — the printing system."),
    (
        "geoclue",
        "GeoClue — provides geolocation to apps that request it.",
    ),
    (
        "curl",
        "curl — command-line tool for transferring data over network protocols.",
    ),
    ("wget", "wget — command-line network downloader."),
    ("ssh", "OpenSSH client — secure remote shell / tunneling."),
    ("firefox", "Mozilla Firefox web browser."),
    ("chrome", "Google Chrome web browser."),
    ("chromium", "Chromium web browser."),
];

/// What a process is, assembled from LOCAL sources only (curated map, man `whatis`, the owning
/// dpkg package). No network — Sluice stays quiet (SEC-007).
#[derive(Serialize)]
struct ProcessInfo {
    name: String,
    path: String,
    package: Option<String>,
    version: Option<String>,
    summary: String,
    source: String,
}

/// The app/binary version, resolved from LOCAL sources only (no network, SEC-007): the owning dpkg
/// package's `${Version}`, or a Snap's version from `snap list`. `None` when it can't be determined.
fn resolve_version(path: &str, package: &Option<String>) -> Option<String> {
    if path.starts_with("/snap/") {
        // /snap/<name>/<rev>/... — the human version comes from `snap list <name>`.
        let snap = path.split('/').nth(2)?;
        let ver = run_stdout("snap", &["list", snap])
            .and_then(|o| o.lines().nth(1).map(|l| l.to_string()))
            .and_then(|l| l.split_whitespace().nth(1).map(|s| s.to_string()))
            .filter(|s| !s.is_empty())?;
        return Some(format!("{ver} (snap)"));
    }
    package
        .as_ref()
        .and_then(|p| run_stdout("dpkg-query", &["-W", "-f=${Version}", "--", p]))
        .and_then(|o| o.lines().next().map(|l| l.trim().to_string()))
        .filter(|s| !s.is_empty())
}

/// Async wrapper: the lookup shells out to dpkg/whatis/snap (blocking), and it's now called for
/// every app + security row — running it on the UI thread froze the app (#34). Run it on the
/// blocking pool so the main thread stays responsive.
#[tauri::command]
async fn describe_process(path: String) -> ProcessInfo {
    tauri::async_runtime::spawn_blocking(move || describe_process_blocking(path))
        .await
        .unwrap_or_else(|_| ProcessInfo {
            name: String::new(),
            path: String::new(),
            package: None,
            version: None,
            summary: String::new(),
            source: "none".to_string(),
        })
}

fn describe_process_blocking(path: String) -> ProcessInfo {
    let name = basename(&path).to_string();

    let curated = CURATED_PROCESSES
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, d)| d.to_string());

    // Owning package (Debian/Ubuntu): `dpkg -S <path>` -> "pkg: /the/path".
    let package = run_stdout("dpkg", &["-S", "--", &path])
        .and_then(|o| o.lines().next().map(|l| l.to_string()))
        .and_then(|l| l.split(':').next().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty() && !s.contains(' '));

    // Man-page one-liner: `whatis curl` -> "curl (1) - transfer a URL".
    let whatis = run_stdout("whatis", &["--", &name])
        .and_then(|o| o.lines().next().map(|l| l.to_string()))
        .and_then(|l| l.split_once(" - ").map(|(_, d)| d.trim().to_string()))
        .filter(|s| !s.is_empty());

    // Package short description.
    let pkg_desc = package
        .as_ref()
        .and_then(|p| run_stdout("dpkg-query", &["-W", "-f=${Description}", "--", p]))
        .and_then(|d| d.lines().next().map(|l| l.trim().to_string()))
        .filter(|s| !s.is_empty());

    let (summary, source) = if let Some(c) = curated {
        (c, "sluice")
    } else if let Some(w) = whatis {
        (w, "man")
    } else if let Some(d) = pkg_desc {
        (d, "package")
    } else if path.starts_with("/snap/") {
        (format!("{name} (a Snap app)."), "snap")
    } else {
        (format!("No local description found for {name}."), "none")
    };

    let version = resolve_version(&path, &package);

    ProcessInfo {
        name,
        path,
        package,
        version,
        summary,
        source: source.to_string(),
    }
}

/// Path to the per-user XDG autostart entry that launches the Sluice UI at login (#31).
fn autostart_desktop_path() -> Option<std::path::PathBuf> {
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(cfg.join("autostart").join("sluice-ui.desktop"))
}

/// The command the autostart entry should run: the installed system binary if present, else the
/// currently-running executable (so it also works for a from-source / dev build).
fn autostart_exec() -> String {
    if std::path::Path::new("/usr/bin/sluice-ui").exists() {
        return "/usr/bin/sluice-ui".to_string();
    }
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sluice-ui".to_string())
}

/// Whether Sluice is set to start at login (the autostart entry exists and isn't disabled).
#[tauri::command]
fn get_autostart() -> bool {
    let Some(path) = autostart_desktop_path() else {
        return false;
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => !s.lines().any(|l| {
            l.trim()
                .eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
        }),
        Err(_) => false,
    }
}

/// Enable or disable start-at-login by writing/removing the XDG autostart entry. Unprivileged,
/// per-user; launches the UI with `--hidden` so it starts in the tray. The engine (the firewall)
/// autostarts on boot via systemd independently of this.
#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    let path = autostart_desktop_path().ok_or("cannot resolve the XDG config directory")?;
    if enabled {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let entry = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Sluice\n\
             Comment=Application firewall & network monitor\n\
             Exec={} --hidden\n\
             Icon=sluice-ui\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            autostart_exec()
        );
        std::fs::write(&path, entry).map_err(|e| e.to_string())?;
    } else if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e.to_string());
        }
    }
    Ok(())
}

/// Decisions within a time window (FR-007). `since` (Unix ms) = 0 means "all / live".
#[tauri::command]
fn history_window(state: State<AppState>, since: i64, limit: Option<u32>) -> Vec<FeedRow> {
    let n = limit.unwrap_or(2000).min(5000);
    state
        .history
        .lock()
        .ok()
        .and_then(|h| h.window(since.max(0), n).ok())
        .unwrap_or_default()
}

/// Clear Sluice's stored history (FR-092). Does not touch engine rules.
#[tauri::command]
fn history_clear(state: State<AppState>) -> bool {
    state
        .history
        .lock()
        .ok()
        .map(|h| h.clear().is_ok())
        .unwrap_or(false)
}

/// Engine connection state shipped to the header indicator (FR-083).
#[derive(Serialize)]
struct EngineStatus {
    connected: bool,
    daemon_version: String,
    uptime_secs: u64,
    /// Seconds since the daemon last contacted us (0 if never).
    last_seen_secs: u64,
    pings: u64,
}

fn now_unix_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Run a command and return trimmed stdout (None if it couldn't spawn or printed nothing).
/// We capture stdout regardless of exit code (e.g. `systemctl is-active` prints "inactive"
/// with a non-zero status).
fn run_stdout(prog: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(prog)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Report whether the Sluice engine is currently connected (its connection-event stream is up).
#[tauri::command]
fn engine_status(state: State<AppState>) -> EngineStatus {
    EngineStatus {
        connected: state.engine_connected.load(Ordering::Relaxed),
        daemon_version: "Sluice engine".to_string(),
        uptime_secs: 0,
        last_seen_secs: 0,
        pings: 0,
    }
}

/// Whether Sluice is sourcing from the Sluice engine (E3.1). The frontend uses this to restrict
/// the decide dialog to engine-supported rules (host/connection scope, no durations). Always
/// true now that the engine is the only backend.
#[tauri::command]
fn engine_mode() -> bool {
    true
}

/// Bring the window to the front — used by the notification-click handler.
#[tauri::command]
fn focus_window(app: tauri::AppHandle) {
    show_main(&app);
}

/// Last path component of a process path (for compact display).
fn basename(path: &str) -> &str {
    path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path)
}

/// Show + focus the main window (from the tray).
fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// Build the system tray (FR-080): an icon with Show / Quit. On GNOME the menu is the reliable
/// interaction (left-click events aren't delivered by the appindicator backend).
/// Build the tray menu (Show / Quit). Kept as a builder so it can be rebuilt verbatim on each
/// refresh — re-publishing an identical menu is what forces the AppIndicator host to re-fetch
/// labels (see `refresh_tray_menu`).
fn tray_menu(app: &tauri::AppHandle) -> tauri::Result<tauri::menu::Menu<tauri::Wry>> {
    let show_i = MenuItemBuilder::with_id("show", "Show Sluice").build(app)?;
    let quit_i = MenuItemBuilder::with_id("quit", "Quit Sluice").build(app)?;
    MenuBuilder::new(app)
        .item(&show_i)
        .separator()
        .item(&quit_i)
        .build()
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let _tray = TrayIconBuilder::with_id("sluice")
        .icon(tauri::image::Image::from_bytes(include_bytes!(
            "../icons/icon.png"
        ))?)
        .tooltip("Sluice — network activity")
        .menu(&tray_menu(app)?)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Re-publish the tray menu so the AppIndicator host re-fetches item labels. The Ubuntu
/// `ubuntu-appindicators` extension intermittently renders blank menu labels after a GNOME Shell
/// / extension reload; re-setting the menu emits a DBusMenu layout update so the labels reappear
/// on their own — no manual extension toggle. Must run on the main (GTK) thread; no-op if the
/// tray isn't present (e.g. no AppIndicator extension at all).
fn refresh_tray_menu(app: &tauri::AppHandle) {
    if let Some(tray) = app.tray_by_id("sluice") {
        match tray_menu(app) {
            Ok(menu) => {
                let _ = tray.set_menu(Some(menu));
            }
            Err(e) => tracing::warn!(error = %e, "tray menu refresh failed"),
        }
    }
}

fn parse_action(s: &str) -> Option<Action> {
    match s {
        "allow" => Some(Action::Allow),
        "deny" => Some(Action::Deny),
        "reject" => Some(Action::Reject),
        _ => None,
    }
}

fn parse_scope(s: &str) -> Scope {
    match s {
        "connection" => Scope::Connection,
        "app_any" => Scope::AppToAny,
        "host_any" => Scope::HostToAny,
        _ => Scope::AppToHost,
    }
}

/// Result of a rule write, surfaced to the UI.
#[derive(Serialize)]
struct RuleResult {
    ok: bool,
    message: String,
}

/// Hosts Sluice must NEVER write a block rule for — blocking core local networking would cut off
/// the machine itself, which a single misclick must not be able to do. An empty host is also
/// protected (too broad to block safely).
///
/// Matching is EXACT, not substring (SEC-010): `localhost`/`*.localhost`, and any address that
/// parses as a loopback IP (127.0.0.0/8, ::1). A substring test wrongly protected attacker-named
/// hosts like `localhost.evil.com` and real public IPv6 addresses containing `::1`, refusing to
/// block hosts the user legitimately wants blocked (a fail-open on standing rules).
fn is_protected_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    if h.is_empty() {
        return true; // too broad to block safely
    }
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    // Loopback IPs only — parsed, so "::1" never matches "2001:db8::1", etc.
    matches!(h.parse::<std::net::IpAddr>(), Ok(ip) if ip.is_loopback())
}

/// Decide-in-place from a feed row (FR-013 block / FR-010 allow): map the given host+port onto an
/// engine IP+port rule (block = SetRule; allow = remove a matching block). `app`/`duration` aren't
/// used by the engine (it's destination-keyed, no durations) but stay in the signature for the
/// frontend's call shape. Never holds traffic.
#[tauri::command]
fn create_rule(
    state: State<AppState>,
    action: String,
    scope: String,
    app: String,
    host: String,
    port: u32,
    duration: String,
) -> RuleResult {
    let _ = (app, duration); // engine rules are destination-keyed with no duration
    let Some(action) = parse_action(&action) else {
        return RuleResult {
            ok: false,
            message: "invalid action".to_string(),
        };
    };

    // Hard safety: never let a (mis)click write a block that would strand the machine (SEC-005) —
    // refuse blocking localhost/loopback (and an empty host, too broad to block safely).
    if !action.is_allow() && is_protected_host(&host) {
        let shown = if host.trim().is_empty() {
            "(empty host)".to_string()
        } else {
            host.trim().to_string()
        };
        tracing::warn!(target = %shown, "refused to block a protected target");
        return RuleResult {
            ok: false,
            message: format!("refused: '{shown}' is protected — Sluice won't block critical hosts"),
        };
    }

    // The Sluice engine's rule map is IP+port keyed: block = SetRule(ip, port|0); allow = remove a
    // matching block.
    engine_create_rule(&state.engine_uds, action, parse_scope(&scope), &host, port)
}

/// List all engine rules for the panel.
#[tauri::command]
fn list_rules(state: State<AppState>) -> Vec<RuleDto> {
    engine_list_rules(&state.engine_uds).unwrap_or_default()
}

/// Inbound posture (E6.1) from the engine.
#[tauri::command]
fn get_inbound(state: State<AppState>) -> InboundDto {
    engine_get_inbound(&state.engine_uds).unwrap_or(InboundDto {
        enforce: false,
        allow: Vec::new(),
    })
}

/// Replace the inbound posture (enforce + allow-list). Enabling enforce is gated by a confirm
/// dialog in the UI (informational — you're responsible for allowing SSH/services you need).
#[tauri::command]
fn set_inbound(state: State<AppState>, enforce: bool, allow: Vec<PortDto>) -> RuleResult {
    match engine_set_inbound(&state.engine_uds, enforce, allow) {
        Ok(()) => RuleResult {
            ok: true,
            message: if enforce {
                "inbound enforcing".to_string()
            } else {
                "inbound observe".to_string()
            },
        },
        Err(e) => RuleResult {
            ok: false,
            message: e,
        },
    }
}

/// Apps view (permissions control panel): every app that's reached the network, with activity
/// from history. The engine has no app-scoped rules (it's destination-keyed), so every app's
/// posture is "monitoring" with no rule count — per-host control lives in the per-app drill-down.
#[tauri::command]
fn apps_inventory(state: State<AppState>) -> Vec<AppInventory> {
    let mut apps = state
        .history
        .lock()
        .ok()
        .and_then(|h| h.apps().ok())
        .unwrap_or_default();
    for a in &mut apps {
        a.posture = "monitoring".to_string();
        a.rules = 0;
    }
    apps
}

/// Destinations permissions view (#11): every destination reached, with real posture from the
/// engine's deny rules (a dest is "blocked" when a deny rule covers one of its IPs). Activity
/// from history; posture overlaid from the engine. Counts are connections, not bytes.
#[tauri::command]
fn dests_inventory(state: State<AppState>) -> Vec<DestInventory> {
    let mut dests = state
        .history
        .lock()
        .ok()
        .and_then(|h| h.dests().ok())
        .unwrap_or_default();
    // The set of IPs the engine currently denies (best-effort; if the engine is unreachable we
    // simply show everything as monitoring rather than fail the view).
    let blocked: std::collections::HashSet<String> = engine_list_rules(&state.engine_uds)
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.host)
        .collect();
    for d in &mut dests {
        d.rules = d.ips.iter().filter(|ip| blocked.contains(*ip)).count() as u32;
        d.posture = if d.rules > 0 { "blocked" } else { "monitoring" }.to_string();
    }
    dests
}

/// Remove an engine rule (in-app undo). Removing a block reduces protection — the confirm dialog
/// (SEC-005) already gated this in the UI before we got here.
#[tauri::command]
fn remove_rule(state: State<AppState>, name: String) -> RuleResult {
    match engine_remove_rule(&state.engine_uds, &name) {
        Ok(()) => RuleResult {
            ok: true,
            message: "rule removed".to_string(),
        },
        Err(e) => RuleResult {
            ok: false,
            message: e,
        },
    }
}

fn main() {
    // WebKitGTK's DMABUF renderer hangs the window (dead controls / no input) on some Wayland +
    // GPU/driver combos. Disabling it before the webview initializes is the standard fix; set it
    // only if the user hasn't chosen their own value. Must run before any GTK/WebKit init.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Sluice runs on the Sluice engine: the feed comes from the engine's connection-event stream
    // over UDS (default socket; SLUICE_ENGINE_UDS overrides for dev). Every connection is shown
    // in the feed; the engine enforces. Sluice never holds traffic.
    let engine_uds = std::env::var("SLUICE_ENGINE_UDS")
        .unwrap_or_else(|_| "/run/sluice/engine.sock".to_string());
    let engine_connected = Arc::new(AtomicBool::new(false));
    let (engine_feed_tx, engine_feed_rx) = tokio::sync::broadcast::channel::<FeedEvent>(4096);
    tracing::info!(uds = %engine_uds, "feed sourced from the Sluice engine over UDS");
    let mut feed = engine_feed_rx;

    // Local history store (DEC-005). Degrades to in-memory if the data dir is unwritable.
    let history = {
        let dir = ensure_data_dir();
        let db = dir.join("history.db");
        let h = History::open(&db);
        tracing::info!(path = %db.display(), persistent = h.persistent, "history store opened");
        Arc::new(Mutex::new(h))
    };

    // History retention (#32): prune older-than now, then keep pruning on a timer.
    let rd = load_retention();
    prune_history(&history, rd);
    let retention_days = Arc::new(Mutex::new(rd));
    {
        let h = Arc::clone(&history);
        let r = Arc::clone(&retention_days);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1800));
            let days = r.lock().map(|g| *g).unwrap_or(0);
            prune_history(&h, days);
        });
    }

    // Clone for the feed→history bridge (the original moves into managed state).
    let bridge_history = Arc::clone(&history);

    tauri::Builder::default()
        // Single-instance guard (#34): a second launch (app menu, login autostart, post-update
        // restart) focuses the running window instead of spawning a duplicate sluice-ui / tray icon.
        // Must be the first plugin registered.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            history,
            retention_days,
            engine_uds: engine_uds.clone(),
            engine_connected: Arc::clone(&engine_connected),
        })
        .invoke_handler(tauri::generate_handler![
            engine_mode,
            get_inbound,
            set_inbound,
            create_rule,
            list_rules,
            remove_rule,
            focus_window,
            engine_status,
            history_recent,
            history_window,
            history_clear,
            app_history,
            host_history,
            dest_history,
            port_history,
            dests_inventory,
            usage,
            apps_inventory,
            get_retention,
            set_retention,
            security_events,
            security_clear,
            geo_country,
            rdns,
            open_url,
            app_version,
            check_for_update,
            download_and_apply_update,
            restart_app,
            describe_process,
            get_autostart,
            set_autostart
        ])
        .on_window_event(|window, event| match event {
            // Close = hide to tray and keep running (FR-080). Quit from the tray menu to exit.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            // Returning to the app is a good moment to heal blank AppIndicator labels (which
            // tend to drop after a shell/extension reload, e.g. unlock) — re-publish the menu.
            WindowEvent::Focused(true) => refresh_tray_menu(window.app_handle()),
            _ => {}
        })
        .setup(move |app| {
            let app_handle = app.handle().clone();
            if let Err(e) = build_tray(&app_handle) {
                tracing::warn!(error = %e, "tray unavailable; continuing without it");
            }
            // Window/taskbar icon = the same signal-rings mark as the tray.
            if let Some(win) = app.get_webview_window("main") {
                if let Ok(icon) =
                    tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png"))
                {
                    let _ = win.set_icon(icon);
                }
                // The window is created VISIBLE by default: a deferred first-show breaks WebKitGTK's
                // window controls on Wayland (laggy/unresponsive min/max/close). For the --hidden
                // login-autostart entry, hide it after creation so it starts in the tray — a later
                // tray "Show" reuses the already-initialized surface and stays responsive.
                if std::env::args().any(|a| a == "--hidden") {
                    let _ = win.hide();
                }
            }
            // The AppIndicator host can latch onto an empty menu layout if we publish before it
            // has subscribed; re-publish once shortly after startup so labels populate reliably.
            {
                let h = app_handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1500));
                    let h2 = h.clone();
                    let _ = h.run_on_main_thread(move || refresh_tray_menu(&h2));
                });
            }
            tracing::info!("Sluice UI up — running on the Sluice engine.");
            // Run the engine link + feed→webview bridge on a dedicated tokio runtime,
            // independent of Tauri's event loop on the main thread.
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("build tokio runtime");
                rt.block_on(async move {
                    // Stream connection events from the Sluice engine into the feed pipeline, and
                    // track the link state for the status pill (reconnects internally).
                    {
                        let tx = engine_feed_tx.clone();
                        let connected = Arc::clone(&engine_connected);
                        let uds = engine_uds.clone();
                        let app_handle = app_handle.clone();
                        tokio::spawn(async move {
                            run_engine_client(uds, tx, connected, app_handle).await
                        });
                    }

                    // Live bandwidth sampler (FR-060): read /proc/net/dev ~1 Hz, diff the
                    // cumulative byte counters, and emit bytes/sec in/out. Unprivileged, local.
                    {
                        let app_handle = app_handle.clone();
                        tokio::spawn(async move {
                            let mut prev = netstat::read_totals();
                            let mut ticker = tokio::time::interval(Duration::from_secs(1));
                            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                            loop {
                                ticker.tick().await;
                                let now = netstat::read_totals();
                                if let (Some((prx, ptx)), Some((rx, tx))) = (prev, now) {
                                    // saturating_sub guards counter resets (e.g. an iface going down).
                                    let _ = app_handle.emit(
                                        "throughput",
                                        Throughput {
                                            rx_bps: rx.saturating_sub(prx),
                                            tx_bps: tx.saturating_sub(ptx),
                                            at_ms: now_unix_ms().to_string(),
                                        },
                                    );
                                }
                                prev = now;
                            }
                        });
                    }

                    // Batch feed events into one emit per tick so a busy machine can't flood
                    // Tauri's main thread (which made the window unresponsive).
                    let mut batch: Vec<FeedRow> = Vec::new();
                    let mut ticker = tokio::time::interval(Duration::from_millis(150));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

                    // New-app detection (FR-051): seed "known" from persisted history so only
                    // genuinely-new binaries alert. A startup warmup suppresses notifications for
                    // the initial burst (so a fresh/empty history doesn't fire dozens at once);
                    // we still learn the baseline during it.
                    let mut known: HashSet<String> = match bridge_history.lock() {
                        Ok(h) => h
                            .distinct_processes()
                            .unwrap_or_default()
                            .into_iter()
                            .collect(),
                        Err(_) => HashSet::new(),
                    };
                    let startup = std::time::Instant::now();
                    let mut last_newapp: Option<std::time::Instant> = None;

                    loop {
                        tokio::select! {
                            msg = feed.recv() => match msg {
                                Ok(ev) => {
                                    let mut fr = FeedRow::from_event(&ev);
                                    // First-ever connection from this binary?
                                    if !matches!(ev.state, ConnState::Pending)
                                        && !fr.process.is_empty()
                                        && known.insert(fr.process.clone())
                                    {
                                        fr.first_seen = true;
                                        let now = std::time::Instant::now();
                                        let warm =
                                            now.duration_since(startup) >= Duration::from_secs(15);
                                        // After warmup (so the baseline-learning burst isn't logged),
                                        // every genuinely-new app is a security event (FR-053)…
                                        if warm {
                                            record_alert(
                                                &app_handle,
                                                &bridge_history,
                                                AlertRow {
                                                    at_ms: ev.at_unix_ms.to_string(),
                                                    level: "info".to_string(),
                                                    priority: "low".to_string(),
                                                    what: "new-app".to_string(),
                                                    text: format!(
                                                        "First network activity from {}",
                                                        basename(&fr.process)
                                                    ),
                                                    process: fr.process.clone(),
                                                    host: fr.host.clone(),
                                                },
                                            );
                                            // …and a desktop notification, throttled to avoid a burst.
                                            let due = last_newapp.is_none_or(|t| {
                                                now.duration_since(t) >= Duration::from_secs(3)
                                            });
                                            if due {
                                                last_newapp = Some(now);
                                                let _ = app_handle
                                                    .notification()
                                                    .builder()
                                                    .title("Sluice — new app online")
                                                    .body(format!(
                                                        "{} made its first network connection → {}",
                                                        basename(&fr.process),
                                                        fr.host
                                                    ))
                                                    .show();
                                            }
                                        }
                                    }
                                    batch.push(fr);
                                }
                                Err(RecvError::Lagged(n)) => {
                                    tracing::warn!(dropped = n, "feed lagged; some events skipped");
                                }
                                Err(RecvError::Closed) => break,
                            },
                            _ = ticker.tick() => {
                                if !batch.is_empty() {
                                    let _ = app_handle.emit("feed-batch", &batch);
                                    // Persist resolved decisions (skip transient pending rows).
                                    let resolved: Vec<&FeedRow> =
                                        batch.iter().filter(|r| r.state != "pending").collect();
                                    if !resolved.is_empty() {
                                        if let Ok(mut h) = bridge_history.lock() {
                                            if let Err(e) = h.insert(&resolved) {
                                                tracing::warn!(error = %e, "history insert failed");
                                            }
                                        }
                                    }
                                    batch.clear();
                                }
                            }
                        }
                    }
                    if !batch.is_empty() {
                        let _ = app_handle.emit("feed-batch", &batch);
                    }
                });
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Sluice");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_hosts_can_never_be_blocked() {
        // Core local networking (and an empty, too-broad host) must always be refused as a
        // block target, so a misclick can never strand the machine. Matching is exact/loopback-IP.
        for h in [
            "localhost",
            "LOCALHOST",
            "foo.localhost",
            "127.0.0.1",
            "127.0.0.53", // systemd-resolved stub (loopback)
            "::1",
            "", // empty host is too broad to block safely
        ] {
            assert!(is_protected_host(h), "{h:?} must be protected");
        }
        // Ordinary remote hosts — including ones that merely *contain* a protected substring —
        // are legitimate, blockable targets (the old substring match fail-opened on these).
        for h in [
            "example.com",
            "cloudflare.com",
            "93.184.216.34",
            "github.com",
            "localhost.evil.com",
            "2001:db8::1",
            "8.8.8.8",
        ] {
            assert!(!is_protected_host(h), "{h:?} must NOT be protected");
        }
    }

    #[test]
    fn parsers_default_to_the_safer_choice() {
        assert_eq!(parse_action("allow"), Some(Action::Allow));
        assert_eq!(parse_action("deny"), Some(Action::Deny));
        assert_eq!(parse_action("reject"), Some(Action::Reject));
        assert_eq!(parse_action("nonsense"), None);
        // Unknown scope falls back to the *narrowest* (app→host), not a broad one.
        assert!(matches!(parse_scope("app_any"), Scope::AppToAny));
        assert!(matches!(parse_scope("nonsense"), Scope::AppToHost));
    }

    #[test]
    fn version_compare_is_numeric_not_lexical() {
        assert!(version_newer("0.1.8", "0.1.7"));
        assert!(version_newer("0.2.0", "0.1.9"));
        assert!(version_newer("v0.1.10", "0.1.9")); // 10 > 9 numerically, not "10" < "9"
        assert!(!version_newer("0.1.7", "0.1.7")); // equal is not newer
        assert!(!version_newer("0.1.7", "0.1.8")); // older is not newer
        assert!(!version_newer("", "0.1.7"));
    }

    #[test]
    fn deb_assets_are_selected_from_a_release() {
        let v: serde_json::Value = serde_json::json!({
            "tag_name": "v0.1.8",
            "assets": [
                { "name": "Sluice_0.1.8_amd64.deb.sha256",
                  "browser_download_url": "https://example/sha" },
                { "name": "Sluice_0.1.8_amd64.deb",
                  "browser_download_url": "https://example/deb" },
                { "name": "source.tar.gz",
                  "browser_download_url": "https://example/src" }
            ]
        });
        let (name, deb, sha) = select_deb_assets(&v).expect("deb + sha present");
        assert_eq!(name, "Sluice_0.1.8_amd64.deb");
        assert_eq!(deb, "https://example/deb");
        assert_eq!(sha, "https://example/sha");

        // A release with no .deb yields nothing (updater reports it cleanly).
        let empty = serde_json::json!({ "assets": [
            { "name": "notes.txt", "browser_download_url": "https://example/n" }
        ]});
        assert!(select_deb_assets(&empty).is_none());
    }

    #[test]
    fn release_signature_verifies_and_rejects_tampering() {
        use minisign_verify::{PublicKey, Signature};
        // Throwaway-key fixtures (src/testdata/), signed over the exact bytes of test.data.
        let pub_b64 = include_str!("testdata/test.pub")
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with("untrusted comment"))
            .unwrap();
        let sig_text = include_str!("testdata/test.minisig");
        let data: &[u8] = include_bytes!("testdata/test.data");

        let pk = PublicKey::from_base64(pub_b64).unwrap();
        let sig = Signature::decode(sig_text).unwrap();
        // Genuine bytes verify; flipping one byte must fail.
        assert!(pk.verify(data, &sig, false).is_ok());
        let mut tampered = data.to_vec();
        tampered[0] ^= 0x01;
        assert!(pk.verify(&tampered, &sig, false).is_err());

        // The embedded REAL release public key parses, so verification won't break at runtime.
        assert!(PublicKey::from_base64(release_pubkey_b64()).is_ok());
    }
}
