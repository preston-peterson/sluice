//! Passive DNS-response snoop (E4): builds an `IP → hostname` cache from DNS answers so the
//! engine can label connections with the name the app resolved (`cgroup/connect` only sees the
//! IP). Capture is `AF_PACKET` (root, `SOCK_DGRAM` → IP-onward, all interfaces incl. loopback),
//! read-only, **no egress** (SEC-007). Plaintext DNS only — DoH/DoT is encrypted (see E4.1/SNI).
//!
//! IPv4 transport carries both A and AAAA answers, so v6 destinations get names too. The cache
//! is **in-memory only** (browsing PII — never persisted).

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::{Arc, Mutex},
    time::Instant,
};

const ETH_P_IP: u16 = 0x0800;
const DNS_TTL_SECS: u64 = 3600; // forget a mapping after an hour idle
const MAX_ENTRIES: usize = 50_000; // memory bound

/// Shared `IP → (hostname, last-seen)` cache.
#[derive(Clone, Default)]
pub struct DnsCache(Arc<Mutex<HashMap<IpAddr, (String, Instant)>>>);

impl DnsCache {
    /// The most-recent hostname that resolved to `ip`, if still fresh.
    pub fn get(&self, ip: IpAddr) -> Option<String> {
        let mut m = self.0.lock().unwrap();
        if let Some((name, seen)) = m.get(&ip) {
            if seen.elapsed().as_secs() < DNS_TTL_SECS {
                return Some(name.clone());
            }
            m.remove(&ip);
        }
        None
    }

    fn insert(&self, ip: IpAddr, name: String) {
        if name.is_empty() {
            return;
        }
        let mut m = self.0.lock().unwrap();
        if m.len() >= MAX_ENTRIES && !m.contains_key(&ip) {
            m.retain(|_, (_, seen)| seen.elapsed().as_secs() < DNS_TTL_SECS);
            if m.len() >= MAX_ENTRIES {
                return; // give up rather than grow unbounded
            }
        }
        m.insert(ip, (name, Instant::now()));
    }
}

/// Start the snoop on a dedicated thread (blocking `recv`). Failures (e.g. missing privilege)
/// are logged and leave the cache empty — names just won't appear; nothing else breaks.
pub fn spawn_snoop(cache: DnsCache) {
    std::thread::spawn(move || {
        if let Err(e) = run(&cache) {
            eprintln!("[sluice] DNS snoop disabled ({e}); feed will show IPs without names");
        }
    });
}

fn run(cache: &DnsCache) -> std::io::Result<()> {
    // AF_PACKET/SOCK_DGRAM: data starts at the IP header, uniform across link types.
    let proto = ETH_P_IP.to_be() as i32;
    let fd = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_DGRAM, proto) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    eprintln!("[sluice] DNS snoop: capturing UDP/53 responses → IP/host cache");
    let mut buf = [0u8; 4096];
    loop {
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
        if n <= 0 {
            continue;
        }
        if let Some(payload) = ipv4_udp_from_port53(&buf[..n as usize]) {
            for (ip, name) in parse_dns_answers(payload) {
                cache.insert(ip, name);
            }
        }
    }
}

/// If `pkt` (an IPv4 packet) is a UDP datagram with **source** port 53, return its payload.
fn ipv4_udp_from_port53(pkt: &[u8]) -> Option<&[u8]> {
    if pkt.len() < 20 || pkt[0] >> 4 != 4 {
        return None;
    }
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    if ihl < 20 || pkt.len() < ihl + 8 || pkt[9] != 17 {
        return None; // not UDP
    }
    let src_port = u16::from_be_bytes([pkt[ihl], pkt[ihl + 1]]);
    if src_port != 53 {
        return None;
    }
    let udp_len = u16::from_be_bytes([pkt[ihl + 4], pkt[ihl + 5]]) as usize;
    let start = ihl + 8;
    let end = (ihl + udp_len).min(pkt.len());
    if end <= start {
        return None;
    }
    Some(&pkt[start..end])
}

/// Parse a DNS response: return (answer IP, queried name) pairs from A/AAAA records.
fn parse_dns_answers(msg: &[u8]) -> Vec<(IpAddr, String)> {
    let mut out = Vec::new();
    if msg.len() < 12 {
        return out;
    }
    let flags = u16::from_be_bytes([msg[2], msg[3]]);
    if flags & 0x8000 == 0 {
        return out; // not a response
    }
    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;

    let mut off = 12;
    let mut qname = String::new();
    for i in 0..qd {
        let (name, next) = read_name(msg, off);
        if i == 0 {
            qname = name;
        }
        off = next + 4; // qtype(2) + qclass(2)
        if off > msg.len() {
            return out;
        }
    }
    if qname.is_empty() {
        return out;
    }

    for _ in 0..an {
        if off >= msg.len() {
            break;
        }
        let (_name, next) = read_name(msg, off);
        off = next;
        if off + 10 > msg.len() {
            break;
        }
        let rtype = u16::from_be_bytes([msg[off], msg[off + 1]]);
        let rdlen = u16::from_be_bytes([msg[off + 8], msg[off + 9]]) as usize;
        off += 10;
        if off + rdlen > msg.len() {
            break;
        }
        let rdata = &msg[off..off + rdlen];
        match (rtype, rdlen) {
            (1, 4) => out.push((
                IpAddr::V4(Ipv4Addr::new(rdata[0], rdata[1], rdata[2], rdata[3])),
                qname.clone(),
            )),
            (28, 16) => {
                let mut o = [0u8; 16];
                o.copy_from_slice(rdata);
                out.push((IpAddr::V6(Ipv6Addr::from(o)), qname.clone()));
            }
            _ => {}
        }
        off += rdlen;
    }
    out
}

/// Read a (possibly compressed) DNS name; return it lowercased + the offset to continue at in
/// the linear stream (after the first pointer, or after the terminating zero).
fn read_name(msg: &[u8], start: usize) -> (String, usize) {
    let mut labels: Vec<String> = Vec::new();
    let mut off = start;
    let mut end = 0usize;
    let mut jumped = false;
    let mut guard = 0;
    while off < msg.len() {
        let b = msg[off];
        if b & 0xC0 == 0xC0 {
            if off + 1 >= msg.len() {
                break;
            }
            if !jumped {
                end = off + 2;
            }
            off = (((b & 0x3f) as usize) << 8) | msg[off + 1] as usize;
            jumped = true;
            guard += 1;
            if guard > 32 {
                break; // pointer loop guard
            }
        } else if b == 0 {
            if !jumped {
                end = off + 1;
            }
            break;
        } else {
            let l = b as usize;
            off += 1;
            if off + l > msg.len() {
                break;
            }
            if let Ok(s) = std::str::from_utf8(&msg[off..off + l]) {
                labels.push(s.to_ascii_lowercase());
            }
            off += l;
        }
    }
    (labels.join("."), if end != 0 { end } else { off })
}
