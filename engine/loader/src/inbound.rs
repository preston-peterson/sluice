//! Inbound observer (E6.0): watches conntrack **NEW** events over netlink and reports
//! connections where a *remote* peer connected to a *local* address — i.e. inbound, which the
//! `cgroup/connect` hook (outbound only) can't see. **Observe-only**, read-only netlink, no
//! egress; enforcement is E6.1 (nftables). IPv4 + TCP/UDP for v1.
//!
//! Recovery/safety: this only reads kernel events — it changes nothing, so it can't strand the
//! box. (The nftables enforcement that *can* comes later, with its own flush-on-exit recovery.)

use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr},
};

const NETLINK_NETFILTER: i32 = 12;
const NFNLGRP_CONNTRACK_NEW: u32 = 1;

// ctnetlink attribute types (see linux/netfilter/nfnetlink_conntrack.h). Flag bits masked off.
const CTA_TUPLE_ORIG: u16 = 1;
const CTA_TUPLE_IP: u16 = 1;
const CTA_TUPLE_PROTO: u16 = 2;
const CTA_IP_V4_SRC: u16 = 1;
const CTA_IP_V4_DST: u16 = 2;
const CTA_PROTO_NUM: u16 = 1;
const CTA_PROTO_DST_PORT: u16 = 3;

/// One observed inbound connection (remote → local).
pub struct InboundEvent {
    pub peer: Ipv4Addr,  // the remote end that connected in
    pub local: Ipv4Addr, // our address it reached
    pub local_port: u16, // the local port served
    pub proto: u8,       // L4 protocol number (IPPROTO_*): 6 = TCP, 17 = UDP
}

/// Our own addresses (so we can tell inbound from outbound/loopback), via getifaddrs.
pub fn local_ips() -> HashSet<IpAddr> {
    let mut set = HashSet::new();
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return set;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let a = &*cur;
            if !a.ifa_addr.is_null() && (*a.ifa_addr).sa_family as i32 == libc::AF_INET {
                let sin = &*(a.ifa_addr as *const libc::sockaddr_in);
                // s_addr is network byte order; to_ne_bytes gives the address octets.
                set.insert(IpAddr::V4(Ipv4Addr::from(
                    sin.sin_addr.s_addr.to_ne_bytes(),
                )));
            }
            cur = a.ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    set
}

/// Spawn the conntrack-event observer on a dedicated thread. For each inbound NEW connection,
/// `on_event` is called. Failures (e.g. missing privilege) are logged and leave inbound
/// invisible — nothing else breaks.
pub fn spawn<F>(local: HashSet<IpAddr>, on_event: F)
where
    F: Fn(InboundEvent) + Send + 'static,
{
    std::thread::spawn(move || {
        if let Err(e) = run(&local, &on_event) {
            eprintln!("[sluice] inbound observer disabled ({e}); incoming connections won't show");
        }
    });
}

fn run<F: Fn(InboundEvent)>(local: &HashSet<IpAddr>, on_event: &F) -> std::io::Result<()> {
    let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, NETLINK_NETFILTER) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as u16;
    addr.nl_groups = 1 << (NFNLGRP_CONNTRACK_NEW - 1); // subscribe to the NEW multicast group
    let rc = unsafe {
        libc::bind(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as u32,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    eprintln!("[sluice] inbound observer: watching conntrack NEW events (observe-only)");

    let mut buf = [0u8; 8192];
    loop {
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
        if n <= 0 {
            continue;
        }
        let mut off = 0usize;
        let n = n as usize;
        // Walk the netlink messages in this datagram.
        while off + 16 <= n {
            let len = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            if len < 16 || off + len > n {
                break;
            }
            // payload = nfgenmsg (4 bytes) + ctnetlink attributes
            let body = &buf[off + 16..off + len];
            if body.len() > 4 {
                if let Some(ev) = parse_orig_tuple(&body[4..]) {
                    let (src, dst) = (IpAddr::V4(ev.peer), IpAddr::V4(ev.local));
                    // Inbound = reached one of our addresses from a non-local peer.
                    if local.contains(&dst) && !local.contains(&src) {
                        on_event(ev);
                    }
                }
            }
            off += (len + 3) & !3; // NLMSG_ALIGN
        }
    }
}

/// Parse the CTA_TUPLE_ORIG (src/dst IPv4 + proto + ports) from a ctnetlink message body.
fn parse_orig_tuple(attrs: &[u8]) -> Option<InboundEvent> {
    let orig = find_attr(attrs, CTA_TUPLE_ORIG)?;
    let ip = find_attr(orig, CTA_TUPLE_IP)?;
    let proto = find_attr(orig, CTA_TUPLE_PROTO)?;

    let src = Ipv4Addr::from(<[u8; 4]>::try_from(find_attr(ip, CTA_IP_V4_SRC)?).ok()?);
    let dst = Ipv4Addr::from(<[u8; 4]>::try_from(find_attr(ip, CTA_IP_V4_DST)?).ok()?);
    let pnum = *find_attr(proto, CTA_PROTO_NUM)?.first()?;
    if pnum != 6 && pnum != 17 {
        return None; // TCP/UDP only for v1
    }
    let dport = be16(find_attr(proto, CTA_PROTO_DST_PORT)?)?;
    Some(InboundEvent {
        peer: src,
        local: dst,
        local_port: dport,
        proto: pnum,
    })
}

/// Find a netlink attribute by type within an attribute buffer; returns its payload.
fn find_attr(buf: &[u8], want: u16) -> Option<&[u8]> {
    let mut off = 0;
    while off + 4 <= buf.len() {
        let alen = u16::from_ne_bytes(buf[off..off + 2].try_into().ok()?) as usize;
        let atype = u16::from_ne_bytes(buf[off + 2..off + 4].try_into().ok()?) & 0x3fff;
        if alen < 4 || off + alen > buf.len() {
            break;
        }
        if atype == want {
            return Some(&buf[off + 4..off + alen]);
        }
        off += (alen + 3) & !3;
    }
    None
}

fn be16(b: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(<[u8; 2]>::try_from(b.get(..2)?).ok()?))
}
