//! Shared ABI between the eBPF program (`sluice-ebpf`) and the userspace loader
//! (`sluice-engine`). `no_std` so the BPF target can use it too.
//!
//! Scope so far: the connection-event record the kernel program emits over a ring buffer on
//! every `connect(2)`, now carrying the in-kernel verdict (E1). The real engine will grow a
//! richer rule ABI here.
#![no_std]

/// Verdict the kernel applied to a connection (also the `ConnEvent.verdict` value).
pub const VERDICT_ALLOW: u8 = 0;
pub const VERDICT_BLOCK: u8 = 1;

/// One outbound connection attempt, captured in the `cgroup/connect{4,6}` hook.
///
/// `#[repr(C)]` with a fixed field order so the eBPF side and the loader agree on the
/// byte layout with no padding surprises (u32-aligned fields first, then the byte tail with
/// explicit padding → 36 bytes). Addresses are kept in **network byte order**, exactly as
/// the kernel hands them to us; the loader converts to `Ipv4Addr`/`Ipv6Addr` for display.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ConnEvent {
    /// PID of the connecting task (top 32 bits of `bpf_get_current_pid_tgid`).
    pub pid: u32,
    /// UID of the connecting task (low 32 bits of `bpf_get_current_uid_gid`).
    pub uid: u32,
    /// IPv4 destination, network byte order (0 when `family != AF_INET`).
    pub daddr4: u32,
    /// IPv6 destination, network byte order (all-zero when `family != AF_INET6`).
    pub daddr6: [u32; 4],
    /// Destination port, network byte order.
    pub dport: u16,
    /// Address family: `AF_INET` (2) or `AF_INET6` (10).
    pub family: u16,
    /// The verdict the kernel applied: [`VERDICT_ALLOW`] or [`VERDICT_BLOCK`].
    pub verdict: u8,
    /// L4 protocol as `IPPROTO_*` (6 = TCP, 17 = UDP, 1 = ICMP, 58 = ICMPv6, 0 = unknown),
    /// read from the socket in the `cgroup/connect` hook (#14). Lets the UI filter by protocol
    /// and surfaces ICMP (e.g. `ping`, where it uses a connected datagram socket).
    pub protocol: u8,
    /// Explicit padding to a 4-byte multiple; always zeroed (no stack leak to userspace).
    pub _pad: [u8; 2],
}

impl ConnEvent {
    pub const SIZE: usize = core::mem::size_of::<Self>();
}

/// Pack an IPv4 destination + port into the `BLOCKLIST` map key.
///
/// **Single source of truth** for the key so the kernel and userspace never disagree (a
/// silent mismatch = a rule that never matches). Both args are the values as read *natively*
/// from network-byte-order storage:
/// - kernel: `(*sock_addr).user_ip4` and `(*sock_addr).user_port as u16`
/// - userspace: `u32::from_ne_bytes(ip.octets())` and `port.to_be()`
/// (Engine and host share endianness, so the native reads line up.)
#[inline]
pub const fn rule_key4(daddr4_ne: u32, dport_ne: u16) -> u64 {
    ((daddr4_ne as u64) << 16) | (dport_ne as u64)
}

/// The `BLOCKLIST6` map key: an IPv6 destination + port. A u64 can't hold a 128-bit address, so
/// v6 needs a struct key (vs `rule_key4`'s packed u64). Same single-source-of-truth contract:
/// `addr` words and `port` are the values read **natively** from network-byte-order storage so the
/// kernel and userspace produce identical bytes (a mismatch = a rule that never matches):
/// - kernel: `(*sock_addr).user_ip6[i]` (each a `__be32`) and `(*sock_addr).user_port`
/// - userspace: `u32::from_ne_bytes` of each 4-byte octet chunk, and `port.to_be()`
///
/// `#[repr(C)]` with explicit, always-zeroed padding so the key is byte-stable for the map's
/// byte-wise comparison. Size is 20 bytes (16 + 2 + 2), a multiple of 4 — no trailing padding.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Key6 {
    /// IPv6 destination as four 32-bit words, exactly as read from `user_ip6` (network order).
    pub addr: [u32; 4],
    /// Destination port, network byte order.
    pub port: u16,
    /// Explicit padding; always zeroed (the map compares keys byte-wise).
    pub _pad: [u8; 2],
}

/// Pack an IPv6 destination + port into the `BLOCKLIST6` key. See [`Key6`] for the byte contract.
#[inline]
pub const fn rule_key6(addr: [u32; 4], dport_ne: u16) -> Key6 {
    Key6 {
        addr,
        port: dport_ne,
        _pad: [0; 2],
    }
}
