//! sluice-ebpf (E1) — a `cgroup/connect{4,6}` eBPF program that **enforces from rule maps**.
//!
//! E0 proved we can observe every outbound connection with ~1µs added latency. E1 adds
//! enforcement: `connect4` consults the `BLOCKLIST` map (dst IPv4 + port) and **denies**
//! matching connections in-kernel (returns 0 = reject → EPERM); everything else is allowed.
//! Every connection is still emitted to the ring buffer, now tagged with the verdict.
//! `connect6` stays observe-only for now (v6 enforcement is a follow-on).
//!
//! Default posture is allow-all (an empty map blocks nothing) — the userspace control plane
//! adds rules. The BPF "license" string is what lets us call the GPL-only ring-buffer helpers
//! (Sluice is GPL-3.0-or-later, GPL-compatible).
#![no_std]
#![no_main]

use aya_ebpf::{
    helpers::{bpf_get_current_pid_tgid, bpf_get_current_uid_gid},
    macros::{cgroup_sock_addr, map},
    maps::{HashMap, RingBuf},
    programs::SockAddrContext,
};
use sluice_common::{rule_key4, rule_key6, ConnEvent, Key6, VERDICT_ALLOW, VERDICT_BLOCK};

// 256 KiB ring (power of two, as the kernel requires). Shared by both hooks.
#[map]
static EVENTS: RingBuf = RingBuf::with_byte_size(256 * 1024, 0);

// Deny set (IPv4): key = rule_key4(dst_ipv4, dst_port) (network order). Presence ⇒ deny.
// The value byte is reserved for future flags (E1 only checks presence).
#[map]
static BLOCKLIST: HashMap<u64, u8> = HashMap::with_max_entries(4096, 0);

// Deny set (IPv6): key = rule_key6(dst_ipv6, dst_port). Same semantics as BLOCKLIST, separate
// map because a 128-bit address won't fit the u64 v4 key.
#[map]
static BLOCKLIST6: HashMap<Key6, u8> = HashMap::with_max_entries(4096, 0);

const AF_INET: u16 = 2;
const AF_INET6: u16 = 10;

#[cgroup_sock_addr(connect4)]
pub fn connect4(ctx: SockAddrContext) -> i32 {
    handle(&ctx, AF_INET)
}

#[cgroup_sock_addr(connect6)]
pub fn connect6(ctx: SockAddrContext) -> i32 {
    handle(&ctx, AF_INET6)
}

/// Observe the connection, decide a verdict, emit the event, and return the kernel action
/// (1 = allow, 0 = deny). `family` is a constant at each call site, so the v4-only blocklist
/// path and the v6-only address read each compile into just one program.
#[inline(always)]
fn handle(ctx: &SockAddrContext, family: u16) -> i32 {
    // Raw ctx pointer. Direct field reads of bpf_sock_addr are permitted for cgroup/connect
    // programs *only at a constant offset from the unmodified ctx register*.
    let sa = ctx.sock_addr;

    let pid = (bpf_get_current_pid_tgid() >> 32) as u32;
    let uid = bpf_get_current_uid_gid() as u32;
    let dport = unsafe { (*sa).user_port } as u16;
    // L4 protocol from the socket (IPPROTO_*). A single scalar read at a constant offset, like
    // user_port above — the verifier permits it for cgroup/connect programs.
    let protocol = unsafe { (*sa).protocol } as u8;

    let mut ev = ConnEvent {
        pid,
        uid,
        daddr4: 0,
        daddr6: [0; 4],
        dport,
        family,
        verdict: VERDICT_ALLOW,
        protocol,
        _pad: [0; 2],
    };

    if family == AF_INET6 {
        // Read the four IPv6 words individually with constant indices. An array copy
        // (`ev.daddr6 = (*sa).user_ip6`) makes LLVM form a base pointer `ctx + off` and read
        // relative to it, which the verifier rejects ("modified ctx ptr"). Constant-index
        // reads each compile to a direct `*(u32 *)(ctx + const)`.
        ev.daddr6[0] = unsafe { (*sa).user_ip6[0] };
        ev.daddr6[1] = unsafe { (*sa).user_ip6[1] };
        ev.daddr6[2] = unsafe { (*sa).user_ip6[2] };
        ev.daddr6[3] = unsafe { (*sa).user_ip6[3] };
        // Block on an exact (addr, port) rule OR an (addr, any-port) rule (key port = 0), exactly
        // as connect4 does. Reduce each lookup to a bool BEFORE the next call so no map-value
        // pointer is held across the second lookup (the verifier rejects that).
        let mut block = unsafe { BLOCKLIST6.get(&rule_key6(ev.daddr6, dport)) }.is_some();
        if !block {
            block = unsafe { BLOCKLIST6.get(&rule_key6(ev.daddr6, 0)) }.is_some();
        }
        if block {
            ev.verdict = VERDICT_BLOCK;
        }
    } else {
        let daddr4 = unsafe { (*sa).user_ip4 };
        ev.daddr4 = daddr4;
        // Block if there's an exact (ip, port) rule OR an (ip, any-port) rule (key port = 0).
        // Evaluate sequentially (not `a || b`): each lookup's result is reduced to a bool
        // BEFORE the next lookup, so a map-value pointer is never held across the second
        // `bpf_map_lookup_elem` call — which the verifier rejects.
        let mut block = unsafe { BLOCKLIST.get(&rule_key4(daddr4, dport)) }.is_some();
        if !block {
            block = unsafe { BLOCKLIST.get(&rule_key4(daddr4, 0)) }.is_some();
        }
        if block {
            ev.verdict = VERDICT_BLOCK;
        }
    }

    let _ = EVENTS.output(&ev, 0);

    if ev.verdict == VERDICT_BLOCK {
        0 // deny → connect() returns EPERM
    } else {
        1 // allow
    }
}

// Required to call GPL-only helpers (bpf_ringbuf_*); Sluice is GPL-compatible (see the module note).
#[link_section = "license"]
#[used]
static LICENSE: [u8; 4] = *b"GPL\0";

#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
