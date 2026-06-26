//! Unprivileged host throughput sampler for the live bandwidth graph (FR-060).
//!
//! Connection *counts* (what the engine reports) aren't byte volume, so true throughput needs a
//! separate collector. We read `/proc/net/dev` — the kernel's cumulative per-interface byte
//! counters — which is world-readable (no privileges, SEC-001), purely local (no network,
//! SEC-007), and dependency-free. Sampling it ~1 Hz and diffing the totals yields bytes/sec in
//! and out. Loopback (`lo`) is excluded — it isn't "network" traffic.

/// Sum received + transmitted bytes across all non-loopback interfaces from a `/proc/net/dev`
/// dump (cumulative since boot). Header lines and malformed rows are skipped.
pub fn parse_totals(text: &str) -> (u64, u64) {
    let mut rx_total = 0u64;
    let mut tx_total = 0u64;
    for line in text.lines() {
        // Data rows look like "  eth0: <rx_bytes> <rx_pkts> … <tx_bytes> <tx_pkts> …".
        let Some((iface, rest)) = line.split_once(':') else {
            continue; // header lines have no colon
        };
        let iface = iface.trim();
        if iface == "lo" || iface.is_empty() {
            continue;
        }
        let cols: Vec<&str> = rest.split_whitespace().collect();
        // 8 receive fields then 8 transmit fields: bytes are col 0 (rx) and col 8 (tx).
        if cols.len() >= 9 {
            rx_total += cols[0].parse::<u64>().unwrap_or(0);
            tx_total += cols[8].parse::<u64>().unwrap_or(0);
        }
    }
    (rx_total, tx_total)
}

/// Read the current cumulative (rx, tx) byte totals, or `None` if `/proc/net/dev` is unreadable.
pub fn read_totals() -> Option<(u64, u64)> {
    std::fs::read_to_string("/proc/net/dev")
        .ok()
        .map(|s| parse_totals(&s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_non_loopback_bytes_only() {
        let sample = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 12345     100    0    0    0     0          0         0   12345     100    0    0    0     0       0          0
  eth0: 99999     500    0    0    0     0          0         0   88888     400    0    0    0     0       0          0
  wlan0:   1000    10    0    0    0     0          0         0     2000     20    0    0    0     0       0          0
";
        let (rx, tx) = parse_totals(sample);
        assert_eq!(
            rx,
            99999 + 1000,
            "loopback excluded; eth0 + wlan0 rx summed"
        );
        assert_eq!(
            tx,
            88888 + 2000,
            "loopback excluded; eth0 + wlan0 tx summed"
        );
    }

    #[test]
    fn tolerates_empty_or_header_only() {
        assert_eq!(parse_totals(""), (0, 0));
        assert_eq!(
            parse_totals("Inter-|   Receive | Transmit\n face |bytes\n"),
            (0, 0)
        );
    }
}
