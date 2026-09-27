pub fn fixed_ms(value: f64) -> String {
    if (value * 10.0).round().abs() < 1000.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    }
}

pub fn latency_ms(value: f64) -> String {
    if (0.0..0.1).contains(&value) {
        "< 0.1".into()
    } else {
        fixed_ms(value)
    }
}

pub fn added_ms(value: f64) -> String {
    format!(
        "{}{}",
        if (value * 10.0).round() < 0.0 {
            "−"
        } else {
            "+"
        },
        fixed_ms(value.abs())
    )
}

pub fn speed(value: f64) -> String {
    if (value * 10.0).round() >= 10000.0 {
        format!("{value:.0}")
    } else if (value * 100.0).round() >= 10000.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

pub fn rate(bytes_per_sec: f64) -> String {
    let units = ["bit/s", "kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"];
    let bits = bytes_per_sec * 8.0;
    let mut tier = 0;
    while tier < units.len() - 1 && bits >= 1.2 * 1000.0_f64.powi(tier as i32 + 1) {
        tier += 1;
    }
    format!(
        "{} {}",
        speed(bits / 1000.0_f64.powi(tier as i32)),
        units[tier]
    )
}

pub fn bytes(bytes: u64) -> String {
    let units = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut tier = 0;
    while (value * 10.0).round() >= 10000.0 && tier < units.len() - 1 {
        value /= 1000.0;
        tier += 1;
    }
    if tier == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[tier])
    }
}
