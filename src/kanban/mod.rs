//! Optional kanban board: per-project markdown cards, one board server per machine,
//! an HTTP API, a stdio MCP server and hook-based delivery of sent cards.

pub mod board;
pub mod client;
pub mod mcp;
pub mod paths;
pub mod store;

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// (year, month, day, hour, minute, second) in UTC.
pub fn civil(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86400) as i64;
    let rem = (secs % 86400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn iso(secs: u64) -> String {
    let (y, m, d, h, mi, s) = civil(secs);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

pub fn date8(secs: u64) -> String {
    let (y, m, d, ..) = civil(secs);
    format!("{y:04}{m:02}{d:02}")
}

pub fn rand_bytes(n: usize) -> Vec<u8> {
    #[cfg(unix)]
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let mut b = vec![0u8; n];
        if f.read_exact(&mut b).is_ok() {
            return b;
        }
    }
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = Vec::with_capacity(n + 8);
    let mut i = 0u64;
    while out.len() < n {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        );
        h.write_u32(std::process::id());
        out.extend_from_slice(&h.finish().to_le_bytes());
        i += 1;
    }
    out.truncate(n);
    out
}

/// `n` random lowercase hex characters.
pub fn rand_hex(n: usize) -> String {
    rand_bytes(n.div_ceil(2))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..n]
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(date8(1_785_000_000), "20260725");
    }

    #[test]
    fn hex_len() {
        assert_eq!(rand_hex(4).len(), 4);
        assert_eq!(rand_hex(32).len(), 32);
        assert_ne!(rand_hex(16), rand_hex(16));
    }
}
