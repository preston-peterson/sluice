//! Connection helpers shared with the UI.

use crate::pb;

/// Best-effort human-facing destination: resolved host if present, else the dst IP.
pub fn host_of(conn: &pb::Connection) -> String {
    if !conn.dst_host.is_empty() {
        conn.dst_host.clone()
    } else {
        conn.dst_ip.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_prefers_the_resolved_host_then_falls_back_to_ip() {
        let with_host = pb::Connection {
            dst_host: "example.com".to_string(),
            dst_ip: "93.184.216.34".to_string(),
            ..Default::default()
        };
        assert_eq!(host_of(&with_host), "example.com");

        let ip_only = pb::Connection {
            dst_ip: "93.184.216.34".to_string(),
            ..Default::default()
        };
        assert_eq!(host_of(&ip_only), "93.184.216.34");
    }
}
