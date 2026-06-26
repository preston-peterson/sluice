//! Offline IP→country lookup (FR-052) over a DB-IP Lite CSV. No network (SEC-007), no privileges,
//! no extra crates. The database is fetched per-machine into the data dir (see `just geoip`) —
//! it is **not** committed. When it's absent, lookups return `None` and the UI just
//! shows nothing, so geo is a clean optional enhancement.
//!
//! DB-IP Lite is CC-BY-4.0: attribution ("IP geolocation by DB-IP") is required wherever geo is
//! shown — see `crates/sluice-ui/geoip/NOTICE` and the UI's attribution on the country field.
//!
//! CSV shape (one inclusive range per line): `start_ip,end_ip,country_code` — IPv4 and IPv6
//! ranges are non-overlapping and parsed into two sorted tables for binary search.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::OnceLock;

struct GeoDb {
    v4: Vec<(u32, u32, [u8; 2])>,
    v6: Vec<(u128, u128, [u8; 2])>,
}

impl GeoDb {
    fn lookup(&self, ip: IpAddr) -> Option<[u8; 2]> {
        match ip {
            IpAddr::V4(a) => find(&self.v4, u32::from(a)),
            IpAddr::V6(a) => find(&self.v6, u128::from(a)),
        }
    }
}

/// Last range whose start <= key, returned if key is also within its end (ranges are sorted,
/// non-overlapping).
fn find<T: Copy + Ord>(ranges: &[(T, T, [u8; 2])], key: T) -> Option<[u8; 2]> {
    let idx = ranges.partition_point(|&(start, _, _)| start <= key);
    if idx == 0 {
        return None;
    }
    let (_, end, cc) = ranges[idx - 1];
    (key <= end).then_some(cc)
}

fn parse_csv(text: &str) -> GeoDb {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for line in text.lines() {
        let mut parts = line.split(',').map(|s| s.trim().trim_matches('"'));
        let (Some(start), Some(end), Some(cc)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let cb = cc.as_bytes();
        if cb.len() < 2 {
            continue;
        }
        let code = [cb[0].to_ascii_uppercase(), cb[1].to_ascii_uppercase()];
        match (start.parse::<IpAddr>(), end.parse::<IpAddr>()) {
            (Ok(IpAddr::V4(s)), Ok(IpAddr::V4(e))) => v4.push((u32::from(s), u32::from(e), code)),
            (Ok(IpAddr::V6(s)), Ok(IpAddr::V6(e))) => v6.push((u128::from(s), u128::from(e), code)),
            _ => {}
        }
    }
    v4.sort_by_key(|&(s, _, _)| s);
    v6.sort_by_key(|&(s, _, _)| s);
    GeoDb { v4, v6 }
}

/// Where the CSV lives: `$SLUICE_GEOIP_DB`, else `<data dir>/geoip/dbip-country-lite.csv`.
fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("SLUICE_GEOIP_DB") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    crate::data_dir()
        .join("geoip")
        .join("dbip-country-lite.csv")
}

fn db() -> Option<&'static GeoDb> {
    static DB: OnceLock<Option<GeoDb>> = OnceLock::new();
    DB.get_or_init(|| {
        let path = db_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let db = parse_csv(&text);
                tracing::info!(path = %path.display(), v4 = db.v4.len(), v6 = db.v6.len(), "geoip loaded");
                Some(db)
            }
            Err(_) => {
                tracing::info!(path = %path.display(), "geoip db not present; country lookups disabled");
                None
            }
        }
    })
    .as_ref()
}

/// ISO 3166-1 alpha-2 country code for an IP string, or `None` (bad IP / no DB / not found).
pub fn country_code(ip: &str) -> Option<String> {
    let ip: IpAddr = ip.parse().ok()?;
    let cc = db()?.lookup(ip)?;
    Some(cc.iter().map(|&b| b as char).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
1.0.0.0,1.0.0.255,AU
8.8.8.0,8.8.8.255,US
\"2001:4860:4860::\",\"2001:4860:4860::ffff\",US
";

    #[test]
    fn looks_up_v4_in_and_out_of_range() {
        let db = parse_csv(SAMPLE);
        assert_eq!(db.lookup("8.8.8.8".parse().unwrap()), Some([b'U', b'S']));
        assert_eq!(db.lookup("1.0.0.42".parse().unwrap()), Some([b'A', b'U']));
        assert_eq!(db.lookup("9.9.9.9".parse().unwrap()), None);
        assert_eq!(db.lookup("0.0.0.1".parse().unwrap()), None); // before the first range
    }

    #[test]
    fn looks_up_v6() {
        let db = parse_csv(SAMPLE);
        // ::1 and ::ffff are inside [::0, ::ffff]
        assert_eq!(
            db.lookup("2001:4860:4860::1".parse().unwrap()),
            Some([b'U', b'S'])
        );
        // ::1:0 == 0x1_0000 (65536) is just past the range end ::ffff (65535)
        assert_eq!(db.lookup("2001:4860:4860::1:0".parse().unwrap()), None);
        // a different prefix entirely
        assert_eq!(db.lookup("2606:4700::1".parse().unwrap()), None);
    }

    #[test]
    fn tolerates_junk_lines() {
        let db = parse_csv("not,enough\n#comment\n\n1.0.0.0,1.0.0.255,AU\n");
        assert_eq!(db.v4.len(), 1);
    }
}
