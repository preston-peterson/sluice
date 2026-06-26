//! sluice-engine (E3.1) — loader + control plane + event drainer + UI link.
//!
//! Loads `sluice-ebpf`, attaches `connect4`/`connect6` to the cgroup-v2 root, enforces a
//! persistent `BLOCKLIST` of deny rules in-kernel, async-drains the ring buffer, and serves the
//! Sluice UI over a gRPC stream on a hardened UDS — including **decision RPCs** so the UI can
//! add/remove rules (DEC-010 / E3.1). **Requires root** to load/attach BPF.
//!
//! Env:
//! - `SLUICE_RULES`: path to the JSON rule store (default `/var/lib/sluice/rules.json`). Loaded
//!   at startup and on SIGHUP; the decision RPCs rewrite it. (Replaces the old `IP:PORT` text.)
//! - `SLUICE_ENGINE_UDS`: socket to serve the UI on (default `/run/sluice/engine.sock`).
//! - `SLUICE_OWNER_UID`: uid allowed to connect (falls back to `SUDO_UID`); without it the UI
//!   link is disabled (the stdout drain still runs).
//!
//! Safety: default-allow — only listed rules are denied (no lockout risk). RPC-set rules pass an
//! engine-side safelist (never block loopback/DNS-stub/unspecified — the engine can't strand the
//! box even if the UI guard is bypassed); rules loaded directly from the (root-owned) file are
//! trusted as-is. The gRPC link is local-only (UDS), owner-uid only (chown 0600 + `SO_PEERCRED`).
//! All BPF programs auto-detach when this process exits (Ctrl-C).

use std::{
    collections::BTreeMap,
    fs::{File, Permissions},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    pin::Pin,
    ptr,
    sync::{Arc, Mutex},
};

mod container;
mod dns;
mod inbound;
mod nft;

use anyhow::Context as _;
use aya::{
    maps::{HashMap as BpfHashMap, MapData, RingBuf},
    programs::{CgroupAttachMode, CgroupSockAddr},
    Ebpf, Pod,
};
use serde::{Deserialize, Serialize};
use sluice_common::{rule_key4, rule_key6, ConnEvent, Key6, VERDICT_BLOCK};

/// Userspace wrapper over the shared [`Key6`] so it can be a BPF map key. `Key6` lives in the
/// no_std `common` crate (byte-identical to the kernel side), but `aya::Pod` can only be impl'd for
/// a local type — `#[repr(transparent)]` keeps the bytes identical.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct Key6Pod(Key6);
// SAFETY: Key6 is `#[repr(C)]` plain-old-data (u32s + u16 + zeroed padding), no pointers/padding
// surprises; safe to copy to/from the kernel map as raw bytes.
unsafe impl Pod for Key6Pod {}
use sluice_proto::{
    sluice_engine_server::{SluiceEngine, SluiceEngineServer},
    ConnEvent as PbConnEvent, InboundAllow, InboundPolicy, InboundQuery, ListRequest,
    Rule as PbRule, RuleAck, RuleId, RuleList, WatchRequest,
};
use tokio::{
    io::unix::AsyncFd,
    net::UnixListener,
    signal::unix::{signal, SignalKind},
    sync::broadcast,
};
use tokio_stream::{
    wrappers::{BroadcastStream, UnixListenerStream},
    Stream, StreamExt,
};
use tonic::{transport::Server, Request, Response, Status};

const AF_INET6: u16 = 10;
const CGROUP2_ROOT: &str = "/sys/fs/cgroup";
const DEFAULT_UDS: &str = "/run/sluice/engine.sock";
const DEFAULT_RULES: &str = "/var/lib/sluice/rules.json";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Where the compiled BPF object lives. Default points at the sibling ebpf build output;
    // override with SLUICE_BPF_OBJ.
    let obj = std::env::var("SLUICE_BPF_OBJ").unwrap_or_else(|_| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ebpf/target/bpfel-unknown-none/release/sluice-ebpf"
        )
        .to_string()
    });
    eprintln!("[sluice] loading BPF object: {obj}");
    let mut bpf = Ebpf::load_file(&obj).with_context(|| format!("loading {obj}"))?;

    // Attach both connect hooks to the unified cgroup-v2 hierarchy root → covers every task.
    let cgroup = File::open(CGROUP2_ROOT).with_context(|| format!("opening {CGROUP2_ROOT}"))?;
    let mut attached = 0;
    for name in ["connect4", "connect6"] {
        // Warn-and-continue per program: one bad attach shouldn't kill the run or leak the
        // other program's (auto-detaching) link.
        match attach_one(&mut bpf, name, &cgroup) {
            Ok(()) => {
                eprintln!("[sluice] attached {name} -> {CGROUP2_ROOT}");
                attached += 1;
            }
            Err(e) => eprintln!("[sluice] WARN: {name} not attached: {e:#}"),
        }
    }
    if attached == 0 {
        anyhow::bail!("no connect hooks attached");
    }

    // Control plane: the BLOCKLIST map + the persistent rule store that drives it. Shared
    // (behind a Mutex) between the SIGHUP reload here and the decision RPCs in the gRPC server.
    let blocklist: BpfHashMap<_, u64, u8> = BpfHashMap::try_from(
        bpf.take_map("BLOCKLIST")
            .context("BLOCKLIST map not found")?,
    )?;
    let blocklist6: BpfHashMap<_, Key6Pod, u8> = BpfHashMap::try_from(
        bpf.take_map("BLOCKLIST6")
            .context("BLOCKLIST6 map not found")?,
    )?;
    let rules_path = std::env::var("SLUICE_RULES").unwrap_or_else(|_| DEFAULT_RULES.to_string());
    let rules = Arc::new(Mutex::new(RuleState::load(
        blocklist,
        blocklist6,
        PathBuf::from(&rules_path),
    )));

    // Inbound enforcement (E6.1): nftables input chain, off by default. Load the saved posture
    // and apply it (clearing any stale table from a prior crash first).
    let inbound_path = std::env::var("SLUICE_INBOUND")
        .unwrap_or_else(|_| "/var/lib/sluice/inbound.json".to_string());
    let inbound = Arc::new(Mutex::new(nft::Inbound::load(PathBuf::from(&inbound_path))));
    inbound.lock().unwrap().startup();

    // DNS snoop (E4): an IP→hostname cache populated from DNS responses, used to label events.
    let dns_cache = dns::DnsCache::default();
    dns::spawn_snoop(dns_cache.clone());

    // UI link: an event bus the gRPC server fans out to connected clients + the decision RPCs.
    let (events_tx, _) = broadcast::channel::<PbConnEvent>(1024);

    // Inbound observer (E6.0): surface incoming connections (remote → local) in the feed.
    // Observe-only — reads conntrack NEW events, changes nothing.
    {
        let tx = events_tx.clone();
        let dns = dns_cache.clone();
        inbound::spawn(inbound::local_ips(), move |ev| {
            if tx.receiver_count() == 0 {
                return;
            }
            let _ = tx.send(PbConnEvent {
                pid: 0,
                uid: 0,
                dst_ip: ev.peer.to_string(),
                dst_port: ev.local_port as u32,
                family: 2, // AF_INET
                verdict: 0,
                process_path: String::new(),
                comm: String::new(),
                at_unix_ms: now_ms(),
                dst_host: dns.get(IpAddr::V4(ev.peer)).unwrap_or_default(),
                process_args: Vec::new(),
                container: String::new(),
                inbound: true,
                protocol: proto_name(ev.proto).to_string(),
            });
        });
    }
    match owner_uid() {
        Some(uid) => {
            let uds =
                std::env::var("SLUICE_ENGINE_UDS").unwrap_or_else(|_| DEFAULT_UDS.to_string());
            let tx = events_tx.clone();
            let st = Arc::clone(&rules);
            let inb = Arc::clone(&inbound);
            let path = PathBuf::from(&uds);
            tokio::spawn(async move {
                if let Err(e) = serve_engine_grpc(tx, st, inb, &path, uid).await {
                    eprintln!("[sluice] gRPC engine server stopped: {e:#}");
                }
            });
            eprintln!("[sluice] UI link: gRPC on {uds} (owner uid {uid})");
        }
        None => eprintln!("[sluice] no SLUICE_OWNER_UID/SUDO_UID — UI link disabled (stdout only)"),
    }

    let ring = RingBuf::try_from(
        bpf.take_map("EVENTS")
            .context("EVENTS ring buffer not found")?,
    )?;
    let mut async_ring = AsyncFd::new(ring)?;
    let mut sighup = signal(SignalKind::hangup()).context("install SIGHUP handler")?;
    // systemctl stop sends SIGTERM — handle it so we tear down the nftables table (reopen inbound).
    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;

    eprintln!("[sluice] draining connection events (Ctrl-C to stop, SIGHUP to reload rules)…");
    let mut count: u64 = 0;
    let mut blocked: u64 = 0;
    // Container attribution (cache id→name); only the drain touches it (single task).
    let mut containers = container::Containers::default();
    // Don't spam the journal with every allowed connection when run as a service — the events
    // go to the UI over gRPC anyway. Blocks are always logged (security audit); set
    // SLUICE_LOG_CONNS=1 to also log allows (debug).
    let log_conns = std::env::var("SLUICE_LOG_CONNS").is_ok();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\n[sluice] stopping (Ctrl-C); {count} events ({blocked} blocked).");
                break;
            }
            _ = sigterm.recv() => {
                eprintln!("[sluice] stopping (SIGTERM); {count} events ({blocked} blocked).");
                break;
            }
            _ = sighup.recv() => {
                eprintln!("[sluice] SIGHUP — reloading rules from {rules_path}");
                rules.lock().unwrap().reload();
            }
            guard = async_ring.readable_mut() => {
                let mut guard = guard?;
                let rb = guard.get_inner_mut();
                while let Some(item) = rb.next() {
                    if item.len() < ConnEvent::SIZE {
                        continue;
                    }
                    // The ring item is unaligned bytes; copy out a ConnEvent.
                    let ev: ConnEvent =
                        unsafe { ptr::read_unaligned(item.as_ptr() as *const ConnEvent) };
                    count += 1;
                    let is_block = ev.verdict == VERDICT_BLOCK;
                    if is_block {
                        blocked += 1;
                    }
                    if is_block || log_conns {
                        let tag = if is_block { "BLOCK" } else { "allow" };
                        println!("#{:<6} {:<5} pid={:<7} uid={:<6} -> {}", count, tag, ev.pid, ev.uid, fmt_dst(&ev));
                    }

                    // Fan out to the UI only when something is watching — skips the /proc
                    // enrichment cost entirely when no UI is attached.
                    if events_tx.receiver_count() > 0 {
                        let (process_path, comm, process_args) = enrich(ev.pid);
                        let _ = events_tx.send(PbConnEvent {
                            pid: ev.pid,
                            uid: ev.uid,
                            dst_ip: fmt_ip(&ev),
                            dst_port: u16::from_be(ev.dport) as u32,
                            family: ev.family as u32,
                            verdict: ev.verdict as u32,
                            process_path,
                            comm,
                            at_unix_ms: now_ms(),
                            dst_host: dns_cache.get(ev_ip(&ev)).unwrap_or_default(),
                            process_args,
                            container: containers.label(ev.pid).unwrap_or_default(),
                            inbound: false,
                            protocol: proto_name(ev.protocol).to_string(),
                        });
                    }
                }
                guard.clear_ready();
            }
        }
    }

    // Reopen inbound on exit: remove our nftables table (no-op if it wasn't applied).
    if let Err(e) = nft::teardown() {
        eprintln!(
            "[sluice] WARN: nftables teardown failed (run `nft delete table inet sluice`): {e}"
        );
    } else {
        eprintln!("[sluice] inbound table removed; inbound reopened.");
    }
    Ok(())
}

// ----- BPF attach ----------------------------------------------------------------------

/// Load + attach one cgroup/connect program to the cgroup-v2 root.
///
/// Single (flags=0): the canonical cgroup *link* attach. The link API rejects the
/// ALLOW_MULTI/OVERRIDE flags with EINVAL (those are legacy prog_attach only). The engine
/// owns the root cgroup; coexisting with another firewall isn't needed (the E0 measurement
/// stops any conflicting firewall, and in production Sluice replaces it).
fn attach_one(bpf: &mut Ebpf, name: &str, cgroup: &File) -> anyhow::Result<()> {
    let prog: &mut CgroupSockAddr = bpf
        .program_mut(name)
        .with_context(|| format!("program {name} not found in object"))?
        .try_into()?;
    prog.load().with_context(|| format!("loading {name}"))?;
    prog.attach(cgroup, CgroupAttachMode::Single)
        .with_context(|| format!("attaching {name} to {CGROUP2_ROOT}"))?;
    Ok(())
}

// ----- rule store ----------------------------------------------------------------------

/// A persisted deny rule (dst IP v4/v6, optional port; `port == 0` ⇒ any port).
#[derive(Serialize, Deserialize, Clone)]
struct StoredRule {
    ip: String,
    #[serde(default)]
    port: u16,
}

/// Stable rule id, e.g. `v4:1.2.3.4:443` or `v6:[2606:4700::1]:443` (`:0` = any port). Opaque key —
/// the v6 brackets just keep it readable; it's looked up verbatim, never re-parsed.
fn rule_id(ip: &IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(_) => format!("v4:{ip}:{port}"),
        IpAddr::V6(_) => format!("v6:[{ip}]:{port}"),
    }
}

/// The BLOCKLIST map key for an (ipv4, port) rule — the same packing the kernel uses
/// (`sluice_common::rule_key4`): native-read of the network-order address + port.
fn map_key(ip: Ipv4Addr, port: u16) -> u64 {
    rule_key4(u32::from_ne_bytes(ip.octets()), port.to_be())
}

/// The BLOCKLIST6 map key for an (ipv6, port) rule — mirrors `map_key` for v6: each 4-byte octet
/// chunk read natively (matching the kernel's native read of `user_ip6[i]`) + the network-order
/// port. Wrapped as [`Key6Pod`] so it can be a BPF map key.
fn map_key6(ip: Ipv6Addr, port: u16) -> Key6Pod {
    let o = ip.octets();
    let addr = [
        u32::from_ne_bytes([o[0], o[1], o[2], o[3]]),
        u32::from_ne_bytes([o[4], o[5], o[6], o[7]]),
        u32::from_ne_bytes([o[8], o[9], o[10], o[11]]),
        u32::from_ne_bytes([o[12], o[13], o[14], o[15]]),
    ];
    Key6Pod(rule_key6(addr, port.to_be()))
}

/// Refuse rules (from the UI) that would strand the box. Root edits to the file bypass this.
/// Covers both families: `is_loopback` matches 127.0.0.0/8 and ::1; `is_unspecified` 0.0.0.0 and ::.
fn safelisted(ip: IpAddr) -> Option<&'static str> {
    if ip.is_loopback() {
        Some("refused: loopback (would strand local services and the DNS stub)")
    } else if ip.is_unspecified() {
        Some("refused: unspecified address (0.0.0.0 / ::)")
    } else {
        None
    }
}

/// The engine's rule state: the in-kernel BLOCKLIST maps (v4 + v6) + the on-disk JSON mirror.
struct RuleState {
    blocklist: BpfHashMap<MapData, u64, u8>,
    blocklist6: BpfHashMap<MapData, Key6Pod, u8>,
    items: BTreeMap<String, StoredRule>, // id -> rule (sorted ⇒ stable list order)
    path: PathBuf,
}

impl RuleState {
    /// Create from the maps and load any persisted rules into both the maps and `items`.
    fn load(
        blocklist: BpfHashMap<MapData, u64, u8>,
        blocklist6: BpfHashMap<MapData, Key6Pod, u8>,
        path: PathBuf,
    ) -> Self {
        let mut st = Self {
            blocklist,
            blocklist6,
            items: BTreeMap::new(),
            path,
        };
        st.load_from_file();
        st
    }

    /// Drop a rule's key from whichever kernel map owns its address family.
    fn map_remove(&mut self, ip: IpAddr, port: u16) {
        match ip {
            IpAddr::V4(v4) => {
                let _ = self.blocklist.remove(&map_key(v4, port));
            }
            IpAddr::V6(v6) => {
                let _ = self.blocklist6.remove(&map_key6(v6, port));
            }
        }
    }

    /// Read the JSON store and apply each rule to the map (no safelist — the file is
    /// root-trusted; no save — we just read it).
    fn load_from_file(&mut self) {
        let loaded: Vec<StoredRule> = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        for r in loaded {
            if let Ok(ip) = r.ip.parse::<IpAddr>() {
                self.insert_raw(ip, r.port);
            } else {
                eprintln!("[sluice] WARN: skipping bad rule ip {:?}", r.ip);
            }
        }
        eprintln!(
            "[sluice] {} rule(s) loaded from {}",
            self.items.len(),
            self.path.display()
        );
    }

    /// Insert into the right map + `items` (no safelist, no save). Shared by load and set.
    fn insert_raw(&mut self, ip: IpAddr, port: u16) {
        let ok = match ip {
            IpAddr::V4(v4) => self.blocklist.insert(map_key(v4, port), 1u8, 0).is_ok(),
            IpAddr::V6(v6) => self.blocklist6.insert(map_key6(v6, port), 1u8, 0).is_ok(),
        };
        if ok {
            self.items.insert(
                rule_id(&ip, port),
                StoredRule {
                    ip: ip.to_string(),
                    port,
                },
            );
        } else {
            eprintln!("[sluice] WARN: map insert failed for {ip}:{port}");
        }
    }

    /// Write the JSON store (root-owned dir, `0600` file).
    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            let existed = dir.exists();
            std::fs::create_dir_all(dir)?;
            if !existed {
                std::fs::set_permissions(dir, Permissions::from_mode(0o755))?;
            }
        }
        let list: Vec<&StoredRule> = self.items.values().collect();
        let json = serde_json::to_string_pretty(&list).unwrap_or_else(|_| "[]".into());
        std::fs::write(&self.path, json)?;
        std::fs::set_permissions(&self.path, Permissions::from_mode(0o600))?;
        Ok(())
    }

    /// Add a deny rule (RPC path): safelist-checked + persisted.
    fn set(&mut self, ip: IpAddr, port: u16) -> Result<(), String> {
        if let Some(reason) = safelisted(ip) {
            return Err(reason.to_string());
        }
        self.insert_raw(ip, port);
        self.save().map_err(|e| format!("persist failed: {e}"))?;
        Ok(())
    }

    /// Remove a rule by id; persist.
    fn remove(&mut self, id: &str) -> Result<(), String> {
        let Some(r) = self.items.remove(id) else {
            return Err("no such rule".to_string());
        };
        if let Ok(ip) = r.ip.parse::<IpAddr>() {
            self.map_remove(ip, r.port);
        }
        self.save().map_err(|e| format!("persist failed: {e}"))?;
        Ok(())
    }

    /// Re-read the file from scratch (clear current map entries, then reload).
    fn reload(&mut self) {
        for r in std::mem::take(&mut self.items).into_values() {
            if let Ok(ip) = r.ip.parse::<IpAddr>() {
                self.map_remove(ip, r.port);
            }
        }
        self.load_from_file();
    }

    /// All rules as proto messages (action 1 = block; E3.1 has only block rules).
    fn list(&self) -> Vec<PbRule> {
        self.items
            .iter()
            .map(|(id, r)| PbRule {
                id: id.clone(),
                action: 1,
                dst_ip: r.ip.clone(),
                dst_port: r.port as u32,
            })
            .collect()
    }
}

// ----- UI gRPC link --------------------------------------------------------------------

/// uid permitted to connect to the engine socket: explicit `SLUICE_OWNER_UID`, else the
/// `SUDO_UID` we were launched under.
fn owner_uid() -> Option<u32> {
    std::env::var("SLUICE_OWNER_UID")
        .ok()
        .or_else(|| std::env::var("SUDO_UID").ok())
        .and_then(|s| s.parse().ok())
}

/// The engine's gRPC service: streams observed connections and serves the decision RPCs.
struct EngineSvc {
    tx: broadcast::Sender<PbConnEvent>,
    rules: Arc<Mutex<RuleState>>,
    inbound: Arc<Mutex<nft::Inbound>>,
}

type EventStream = Pin<Box<dyn Stream<Item = Result<PbConnEvent, Status>> + Send>>;

#[tonic::async_trait]
impl SluiceEngine for EngineSvc {
    type WatchConnectionsStream = EventStream;

    async fn watch_connections(
        &self,
        _req: Request<WatchRequest>,
    ) -> Result<Response<EventStream>, Status> {
        eprintln!("[sluice] UI subscribed to connection stream");
        let rx = self.tx.subscribe();
        // Drop lagged markers (a slow UI just misses some rows; the feed is a live view).
        let stream = BroadcastStream::new(rx).filter_map(|r| r.ok().map(Ok));
        Ok(Response::new(Box::pin(stream)))
    }

    async fn set_rule(&self, req: Request<PbRule>) -> Result<Response<RuleAck>, Status> {
        let r = req.into_inner();
        let ip = match r.dst_ip.parse::<IpAddr>() {
            Ok(ip) => ip,
            Err(_) => {
                return Ok(Response::new(RuleAck {
                    ok: false,
                    error: format!("bad IP: {}", r.dst_ip),
                }))
            }
        };
        let port = r.dst_port as u16;
        let res = self.rules.lock().unwrap().set(ip, port);
        Ok(Response::new(match res {
            Ok(()) => {
                eprintln!("[sluice]   + block {ip}:{port} (via UI)");
                RuleAck {
                    ok: true,
                    error: String::new(),
                }
            }
            Err(e) => {
                eprintln!("[sluice]   ! rule refused {ip}:{port}: {e}");
                RuleAck {
                    ok: false,
                    error: e,
                }
            }
        }))
    }

    async fn remove_rule(&self, req: Request<RuleId>) -> Result<Response<RuleAck>, Status> {
        let id = req.into_inner().id;
        let res = self.rules.lock().unwrap().remove(&id);
        Ok(Response::new(match res {
            Ok(()) => {
                eprintln!("[sluice]   - rule {id} (via UI)");
                RuleAck {
                    ok: true,
                    error: String::new(),
                }
            }
            Err(e) => RuleAck {
                ok: false,
                error: e,
            },
        }))
    }

    async fn list_rules(&self, _req: Request<ListRequest>) -> Result<Response<RuleList>, Status> {
        let rules = self.rules.lock().unwrap().list();
        Ok(Response::new(RuleList { rules }))
    }

    async fn get_inbound_policy(
        &self,
        _req: Request<InboundQuery>,
    ) -> Result<Response<InboundPolicy>, Status> {
        let cfg = self.inbound.lock().unwrap().get();
        Ok(Response::new(InboundPolicy {
            enforce: cfg.enforce,
            allow: cfg
                .allow
                .into_iter()
                .map(|a| InboundAllow {
                    proto: a.proto,
                    port: a.port as u32,
                })
                .collect(),
        }))
    }

    async fn set_inbound_policy(
        &self,
        req: Request<InboundPolicy>,
    ) -> Result<Response<RuleAck>, Status> {
        let p = req.into_inner();
        let cfg = nft::InboundConfig {
            enforce: p.enforce,
            allow: p
                .allow
                .into_iter()
                .filter_map(|a| {
                    let port = u16::try_from(a.port).ok().filter(|p| *p != 0)?;
                    let proto = if a.proto.eq_ignore_ascii_case("udp") {
                        "udp"
                    } else {
                        "tcp"
                    };
                    Some(nft::AllowPort {
                        proto: proto.to_string(),
                        port,
                    })
                })
                .collect(),
        };
        let res = self.inbound.lock().unwrap().set(cfg);
        Ok(Response::new(match res {
            Ok(()) => {
                eprintln!("[sluice] inbound policy updated (via UI)");
                RuleAck {
                    ok: true,
                    error: String::new(),
                }
            }
            Err(e) => RuleAck {
                ok: false,
                error: e,
            },
        }))
    }
}

/// Serve the engine gRPC service on a hardened UDS. Root creates the socket; it is chowned to
/// the owner uid and set `0600` so the unprivileged UI (and only it) can connect, with a
/// `SO_PEERCRED` check as defence in depth. The parent dir is `0755` so the owner can traverse
/// to the socket. Local-only — no network egress (SEC-007).
async fn serve_engine_grpc(
    tx: broadcast::Sender<PbConnEvent>,
    rules: Arc<Mutex<RuleState>>,
    inbound: Arc<Mutex<nft::Inbound>>,
    path: &Path,
    owner_uid: u32,
) -> anyhow::Result<()> {
    let listener = bind_engine_uds(path, owner_uid)?;
    let incoming = UnixListenerStream::new(listener).filter_map(move |conn| match conn {
        Ok(stream) => match stream.peer_cred() {
            Ok(cred) if cred.uid() == owner_uid || cred.uid() == 0 => Some(Ok(stream)),
            Ok(cred) => {
                eprintln!(
                    "[sluice] rejected gRPC peer uid {} (not owner/root)",
                    cred.uid()
                );
                None
            }
            Err(e) => {
                eprintln!("[sluice] rejected gRPC peer: SO_PEERCRED failed: {e}");
                None
            }
        },
        Err(e) => Some(Err(e)),
    });

    Server::builder()
        .concurrency_limit_per_connection(64)
        .add_service(SluiceEngineServer::new(EngineSvc { tx, rules, inbound }))
        .serve_with_incoming(incoming)
        .await?;
    Ok(())
}

/// Bind the engine socket: `0755` dir (owner can traverse) + `0600` socket chowned to the
/// owner uid (only the owner can connect). Returns the listener.
fn bind_engine_uds(path: &Path, owner_uid: u32) -> anyhow::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        // Only set perms on a dir we actually create — never chmod a pre-existing shared dir
        // (e.g. /tmp), which would clobber its sticky bit. Our default /run/sluice is ours.
        let existed = dir.exists();
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        if !existed {
            std::fs::set_permissions(dir, Permissions::from_mode(0o755))?;
        }
    }
    let _ = std::fs::remove_file(path); // clear any stale socket
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, Permissions::from_mode(0o600))?;
    // chown to the owner so the unprivileged UI can connect; leave gid unchanged (-1).
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let rc = unsafe { libc::chown(c_path.as_ptr(), owner_uid, u32::MAX) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("chown engine socket to owner");
    }
    Ok(listener)
}

// ----- formatting / enrichment ---------------------------------------------------------

/// Best-effort process attribution from `/proc` (root reads it reliably for any pid): the exe
/// path, `comm`, and the command-line args (which identify *which* node/python/etc. is connecting).
/// Empty when the process has already exited.
fn enrich(pid: u32) -> (String, String, Vec<String>) {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    // cmdline is NUL-separated (trailing NUL); split into args.
    let args = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| {
            b.split(|&c| c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect()
        })
        .unwrap_or_default();
    (exe, comm, args)
}

/// The destination as an `IpAddr` (for the DNS-cache lookup).
fn ev_ip(ev: &ConnEvent) -> IpAddr {
    if ev.family == AF_INET6 {
        let mut bytes = [0u8; 16];
        for (i, word) in ev.daddr6.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
        }
        IpAddr::V6(Ipv6Addr::from(bytes))
    } else {
        IpAddr::V4(Ipv4Addr::from(ev.daddr4.to_ne_bytes()))
    }
}

/// Render just the destination IP from an event's network-order address fields.
fn fmt_ip(ev: &ConnEvent) -> String {
    if ev.family == AF_INET6 {
        let mut bytes = [0u8; 16];
        for (i, word) in ev.daddr6.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
        }
        Ipv6Addr::from(bytes).to_string()
    } else {
        Ipv4Addr::from(ev.daddr4.to_ne_bytes()).to_string()
    }
}

/// Render the destination `ip:port` (IPv6 bracketed) for the stdout drain.
fn fmt_dst(ev: &ConnEvent) -> String {
    let port = u16::from_be(ev.dport);
    if ev.family == AF_INET6 {
        format!("[{}]:{}", fmt_ip(ev), port)
    } else {
        format!("{}:{}", fmt_ip(ev), port)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Render an `IPPROTO_*` number (#14) to the lowercase name the UI filters on. Empty for
/// unknown/0 so the UI can treat it as "other" rather than mislabel it.
fn proto_name(num: u8) -> &'static str {
    match num {
        6 => "tcp",
        17 => "udp",
        1 => "icmp",
        58 => "icmpv6",
        _ => "",
    }
}
