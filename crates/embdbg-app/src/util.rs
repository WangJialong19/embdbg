//! Small formatting helpers.

/// A value with `digits` significant digits, or scientific notation outside `1e-3..1e6`.
pub fn format_sig(value: f64, digits: usize) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "+∞" } else { "-∞" }.to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    let abs = value.abs();
    if !(1e-3..1e6).contains(&abs) {
        let decimals = digits.saturating_sub(1);
        let text = format!("{value:.decimals$e}");
        return if text.contains('e') && !text.contains("e-") {
            text.replace('e', "e+")
        } else {
            text
        };
    }
    // One digit before the point already accounts for one significant digit.
    let decimals = (digits as i32 - 1 - abs.log10().floor() as i32).clamp(0, 6) as usize;
    format!("{value:.decimals$}")
}

/// A value with an SI prefix, for example `1.000 MS/s` or `12.5 kB/s`.
pub fn format_rate(value: f64, unit: &str) -> String {
    const PREFIXES: [(&str, f64); 7] = [
        ("G", 1e9),
        ("M", 1e6),
        ("k", 1e3),
        ("", 1.0),
        ("m", 1e-3),
        ("µ", 1e-6),
        ("n", 1e-9),
    ];
    if value.is_nan() || value <= 0.0 {
        // Zero, negative and NaN rates all read the same.
        return format!("0 {unit}");
    }
    // The first prefix the value does not round up and out of: 999 600 reads as 1.00 M, not 1000 k.
    let (prefix, scale) = PREFIXES
        .iter()
        .copied()
        .find(|(_, scale)| value / scale >= 0.9995)
        .unwrap_or(*PREFIXES.last().expect("the prefix table is not empty"));
    format!("{} {}{}", format_sig(value / scale, 3), prefix, unit)
}

/// A moment on the engine's clock, as seconds or as `mm:ss.mmm` once it passes a minute.
pub fn format_clock(t: f64) -> String {
    if !t.is_finite() || t < 0.0 {
        return "—".to_owned();
    }
    if t < 60.0 {
        format!("{t:7.3} s")
    } else {
        let minutes = (t / 60.0).floor();
        format!("{minutes:02.0}:{:06.3}", t - minutes * 60.0)
    }
}

/// A count with thousands separators, for example `12,345,678`.
pub fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_read_as_seconds_then_minutes() {
        assert_eq!(format_clock(1.234), "  1.234 s");
        assert_eq!(format_clock(62.5), "01:02.500");
        assert_eq!(format_clock(f64::NAN), "—");
    }

    #[test]
    fn counts_are_grouped_by_thousands() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(12_345_678), "12,345,678");
    }

    #[test]
    fn significant_digits() {
        // Always the same number of significant digits, so readouts keep their width.
        assert_eq!(format_sig(1.0, 4), "1.000");
        assert_eq!(format_sig(1234.5678, 4), "1235");
        assert_eq!(format_sig(-0.00123456, 4), "-0.001235");
        assert_eq!(format_sig(0.00001234, 3), "1.23e-5");
        assert_eq!(format_sig(12_345_678.0, 3), "1.23e+7");
        assert_eq!(format_sig(0.0, 3), "0");
        assert_eq!(format_sig(f64::NAN, 3), "NaN");
    }

    #[test]
    fn rates_carry_si_prefixes() {
        assert_eq!(format_rate(1_000_000.0, "S/s"), "1.00 MS/s");
        assert_eq!(format_rate(12_345.0, "B/s"), "12.3 kB/s");
        assert_eq!(format_rate(999.0, "S/s"), "999 S/s");
        assert_eq!(format_rate(999_600.0, "帧/s"), "1.000 M帧/s");
        assert_eq!(format_rate(0.0, "S/s"), "0 S/s");
    }
}
