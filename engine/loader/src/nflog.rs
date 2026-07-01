//! Dropped-inbound observer (#23). The inbound enforce ruleset (nft.rs) logs packets that are
//! about to be `policy drop`ped to NFLOG group [`NFLOG_GROUP`]; this reads that group and turns each
//! logged packet into a BLOCKED inbound event, so the feed can show incoming connections that
//! enforcement rejected (conntrack NEW never fires for dropped packets, so this is the only signal).
//!
//! Read-only netlink — it changes nothing, so it can't strand the box. IPv4 + IPv6, TCP/UDP.
//! Idle in observe mode (the `log` rule only exists while enforcing), so it just sees nothing then.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const NETLINK_NETFILTER: i32 = 12;
const NFNL_SUBSYS_ULOG: u16 = 4; // nfnetlink "log" subsystem (5 is OSF — the earlier bug)
const NFULNL_MSG_PACKET: u16 = 0; // kernel → us: a logged packet
const NFULNL_MSG_CONFIG: u16 = 1; // us → kernel: bind a group / set copy mode
const NFULA_CFG_CMD: u16 = 1;
const NFULA_CFG_MODE: u16 = 2;
const NFULNL_CFG_CMD_BIND: u8 = 1;
const NFULNL_CFG_CMD_PF_BIND: u8 = 3;
const NFULNL_COPY_PACKET: u8 = 2;
const NFULA_PAYLOAD: u16 = 9; // the raw packet (from L3), when copy mode is COPY_PACKET
const NLM_F_REQUEST: u16 = 1;

/// The NFLOG multicast group the inbound `log` rule targets. Must match nft.rs.
pub const NFLOG_GROUP: u16 = 5;

/// A dropped inbound packet observed via NFLOG.
pub struct NflogEvent {
    pub peer: IpAddr,    // the remote source that was rejected
    pub local_port: u16, // the local port it tried to reach
    pub proto: u8,       // 6 = TCP, 17 = UDP
}

/// Spawn the NFLOG reader on a dedicated thread; `on_event` fires per dropped inbound packet.
/// Failure (e.g. missing privilege) is logged and leaves blocked-inbound invisible — nothing else breaks.
pub fn spawn<F>(on_event: F)
where
    F: Fn(NflogEvent) + Send + 'static,
{
    std::thread::spawn(move || {
        if let Err(e) = run(&on_event) {
            eprintln!(
                "[sluice] blocked-inbound observer disabled ({e}); dropped inbound won't show"
            );
        }
    });
}

fn run<F: Fn(NflogEvent)>(on_event: &F) -> std::io::Result<()> {
    let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, NETLINK_NETFILTER) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as u16;
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

    // Handshake (mirrors libnetfilter_log): bind the address families, then the group, then set
    // copy mode. The PF_BIND steps are what make the kernel actually deliver logged packets to us.
    send_config(
        fd,
        libc::AF_INET as u8,
        NFULA_CFG_CMD,
        &[NFULNL_CFG_CMD_PF_BIND],
    )?;
    send_config(
        fd,
        libc::AF_INET6 as u8,
        NFULA_CFG_CMD,
        &[NFULNL_CFG_CMD_PF_BIND],
    )?;
    send_config(fd, 0, NFULA_CFG_CMD, &[NFULNL_CFG_CMD_BIND])?;
    let mut mode = Vec::with_capacity(6);
    mode.extend_from_slice(&0x60u32.to_be_bytes()); // copy_range: enough for IP + L4 headers
    mode.push(NFULNL_COPY_PACKET); // copy_mode
    mode.push(0); // _pad
    send_config(fd, 0, NFULA_CFG_MODE, &mode)?;
    eprintln!("[sluice] blocked-inbound observer: reading NFLOG group {NFLOG_GROUP}");

    let mut buf = [0u8; 65536];
    loop {
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
        if n <= 0 {
            continue;
        }
        let n = n as usize;
        let mut off = 0usize;
        while off + 16 <= n {
            let len = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            let ntype = u16::from_ne_bytes(buf[off + 4..off + 6].try_into().unwrap());
            if len < 16 || off + len > n {
                break;
            }
            if ntype == (NFNL_SUBSYS_ULOG << 8) | NFULNL_MSG_PACKET {
                // body = nfgenmsg (4 bytes) + NFULA_* attributes
                let body = &buf[off + 16..off + len];
                if body.len() > 4 {
                    if let Some(pkt) = find_attr(&body[4..], NFULA_PAYLOAD) {
                        if let Some(ev) = parse_packet(pkt) {
                            on_event(ev);
                        }
                    }
                }
            }
            off += (len + 3) & !3; // NLMSG_ALIGN
        }
    }
}

/// Send a one-attribute NFULNL config message for `NFLOG_GROUP` (bind / set mode).
fn send_config(fd: i32, family: u8, atype: u16, payload: &[u8]) -> std::io::Result<()> {
    let attr_len = 4 + payload.len();
    let total = 16 + 4 + ((attr_len + 3) & !3); // nlmsghdr + nfgenmsg + padded attr
    let mut m = vec![0u8; total];
    // nlmsghdr
    m[0..4].copy_from_slice(&(total as u32).to_ne_bytes());
    m[4..6].copy_from_slice(&((NFNL_SUBSYS_ULOG << 8) | NFULNL_MSG_CONFIG).to_ne_bytes());
    m[6..8].copy_from_slice(&NLM_F_REQUEST.to_ne_bytes());
    // seq (8..12) + pid (12..16) left 0
    // nfgenmsg: family (AF_INET/AF_INET6 for PF_BIND, else AF_UNSPEC), version 0, res_id = group (BE)
    m[16] = family;
    m[18..20].copy_from_slice(&NFLOG_GROUP.to_be_bytes());
    // attribute
    m[20..22].copy_from_slice(&(attr_len as u16).to_ne_bytes());
    m[22..24].copy_from_slice(&atype.to_ne_bytes());
    m[24..24 + payload.len()].copy_from_slice(payload);
    let sent = unsafe { libc::send(fd, m.as_ptr() as *const libc::c_void, m.len(), 0) };
    if sent < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Parse a logged IP packet (from NFULA_PAYLOAD) into a dropped-inbound event. TCP/UDP only.
fn parse_packet(p: &[u8]) -> Option<NflogEvent> {
    match p.first()? >> 4 {
        4 => {
            let ihl = (p[0] & 0x0f) as usize * 4;
            if ihl < 20 || p.len() < ihl + 4 {
                return None;
            }
            let proto = p[9];
            if proto != 6 && proto != 17 {
                return None;
            }
            let src = Ipv4Addr::new(p[12], p[13], p[14], p[15]);
            let dport = u16::from_be_bytes([p[ihl + 2], p[ihl + 3]]);
            Some(NflogEvent {
                peer: IpAddr::V4(src),
                local_port: dport,
                proto,
            })
        }
        6 => {
            if p.len() < 44 {
                return None;
            }
            let proto = p[6]; // next header (extension headers not chased in v1)
            if proto != 6 && proto != 17 {
                return None;
            }
            let src: [u8; 16] = p[8..24].try_into().ok()?;
            let dport = u16::from_be_bytes([p[42], p[43]]);
            Some(NflogEvent {
                peer: IpAddr::V6(Ipv6Addr::from(src)),
                local_port: dport,
                proto,
            })
        }
        _ => None,
    }
}

/// Find a netlink attribute by type; returns its payload (flag bits masked off the type).
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

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal IPv4 TCP SYN to 10.0.0.5:22 from 203.0.113.9 (20-byte IP header, no options).
    fn v4_syn() -> Vec<u8> {
        let mut p = vec![0u8; 24];
        p[0] = 0x45; // v4, ihl=5
        p[9] = 6; // TCP
        p[12..16].copy_from_slice(&[203, 0, 113, 9]); // src
        p[16..20].copy_from_slice(&[10, 0, 0, 5]); // dst
        p[20..22].copy_from_slice(&40000u16.to_be_bytes()); // sport
        p[22..24].copy_from_slice(&22u16.to_be_bytes()); // dport
        p
    }

    #[test]
    fn parses_v4_tcp() {
        let ev = parse_packet(&v4_syn()).expect("v4 tcp");
        assert_eq!(ev.peer, "203.0.113.9".parse::<IpAddr>().unwrap());
        assert_eq!(ev.local_port, 22);
        assert_eq!(ev.proto, 6);
    }

    #[test]
    fn parses_v6_udp() {
        let mut p = vec![0u8; 48];
        p[0] = 0x60; // v6
        p[6] = 17; // next header = UDP
        p[8..24].copy_from_slice(&[0x20, 0x01, 0xd, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]); // src 2001:db8::1
        p[24..40].copy_from_slice(&[0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]); // dst fe80::2
        p[42..44].copy_from_slice(&53u16.to_be_bytes()); // dport
        let ev = parse_packet(&p).expect("v6 udp");
        assert_eq!(ev.local_port, 53);
        assert_eq!(ev.proto, 17);
        assert!(ev.peer.is_ipv6());
    }

    #[test]
    fn ignores_non_tcp_udp_and_junk() {
        let mut icmp = vec![0u8; 24];
        icmp[0] = 0x45;
        icmp[9] = 1; // ICMP
        assert!(parse_packet(&icmp).is_none());
        assert!(parse_packet(&[]).is_none());
        assert!(parse_packet(&[0x45]).is_none());
    }
}
