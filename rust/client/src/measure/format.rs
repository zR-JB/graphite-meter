//! Numbers as every view prints them (`api/format.testvectors.json`).

/// Milliseconds: one decimal below 100, whole from there.
pub fn ms(value: f64) -> String {
    match (value * 10.0).round().abs() < 1000.0 {
        true => format!("{value:.1}"),
        false => format!("{value:.0}"),
    }
}

/// A round trip in milliseconds; below 0.1 shows as `< 0.1`.
pub fn latency(value: f64) -> String {
    match (0.0..0.1).contains(&value) {
        true => "< 0.1".into(),
        false => ms(value),
    }
}

/// Added latency in milliseconds, always signed; what rounds to zero is `+0.0`.
pub fn added(value: f64) -> String {
    let sign = if (value * 10.0).round() < 0.0 { '−' } else { '+' };
    format!("{sign}{}", ms(value.abs()))
}

/// A speed in its unit: two decimals below 100, one below 1000, whole from there.
pub fn speed(value: f64) -> String {
    match () {
        _ if (value * 10.0).round() >= 10_000.0 => format!("{value:.0}"),
        _ if (value * 100.0).round() >= 10_000.0 => format!("{value:.1}"),
        _ => format!("{value:.2}"),
    }
}

/// Bytes per second as bits per second, in the largest unit the value reaches 1.2 of.
pub fn rate(bytes_per_sec: f64) -> String {
    const UNITS: [&str; 5] = ["bit/s", "kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"];
    let mut value = bytes_per_sec * 8.0;
    let mut tier = 0;
    while tier + 1 < UNITS.len() && value >= 1200.0 {
        value /= 1000.0;
        tier += 1;
    }
    format!("{} {}", speed(value), UNITS[tier])
}

/// A byte count: whole bytes below 999.5, one decimal in decimal units from there.
pub fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = count as f64;
    let mut tier = 0;
    while tier + 1 < UNITS.len() && (value * 10.0).round() >= 10_000.0 {
        value /= 1000.0;
        tier += 1;
    }
    match tier {
        0 => format!("{count} B"),
        _ => format!("{value:.1} {}", UNITS[tier]),
    }
}
