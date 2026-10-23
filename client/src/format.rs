//! How the page writes numbers, durations, times and colours, in French.

/// A duration of `minutes`, as `27 min`, `2 h 05` or `1 j 3 h`.
pub fn duration(minutes: u32) -> String {
    match minutes {
        0..60 => format!("{minutes} min"),
        60..1440 => format!("{} h {:02}", minutes / 60, minutes % 60),
        _ => format!("{} j {} h", minutes / 1440, minutes % 1440 / 60),
    }
}

/// `value` with `digits` decimals and a decimal comma.
pub fn decimal(value: f32, digits: usize) -> String {
    format!("{value:.digits$}").replace('.', ",")
}

/// A temperature, rounded to the degree.
pub fn degrees(value: f32) -> String {
    format!("{} °C", value.round() as i32)
}

/// How a Wi-Fi signal of `dbm` fares.
pub fn wifi_quality(dbm: i32) -> &'static str {
    match dbm {
        -50.. => "excellent",
        -60..=-51 => "bon",
        -70..=-61 => "moyen",
        _ => "faible",
    }
}

/// The CSS colour of a filament written `RRGGBB` (or `RRGGBBAA`), if it is
/// one.
pub fn css_color(hex: &str) -> Option<String> {
    let rgb = hex.get(..6)?;
    (matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| format!("#{rgb}"))
}

/// The CSS background of a spool of `colors`: one colour, or stripes of
/// each.
pub fn swatch(colors: &[String]) -> Option<String> {
    let colors: Vec<String> = colors.iter().filter_map(|hex| css_color(hex)).collect();
    match colors.len() {
        0 => None,
        1 => Some(colors[0].clone()),
        n => {
            let stops: Vec<String> = colors
                .iter()
                .enumerate()
                .map(|(i, color)| {
                    format!("{color} {:.1}% {:.1}%", 100.0 * i as f64 / n as f64, 100.0 * (i + 1) as f64 / n as f64)
                })
                .collect();
            Some(format!("linear-gradient(90deg, {})", stops.join(", ")))
        }
    }
}

/// `text` with its first letter in capitals.
pub fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The local wall-clock time of `ms` since the epoch, as `14:32`.
pub fn clock_time(ms: f64) -> String {
    let date = js_sys::Date::new(&ms.into());
    format!("{:02}:{:02}", date.get_hours(), date.get_minutes())
}

/// The local time of `ms`, as `14:32` today, `demain 08:10` tomorrow, or
/// `24/10 08:10` on another day.
pub fn moment(ms: f64, now_ms: f64) -> String {
    let date = js_sys::Date::new(&ms.into());
    let today = js_sys::Date::new(&now_ms.into());
    let day = |d: &js_sys::Date| (d.get_full_year(), d.get_month(), d.get_date());
    let tomorrow = js_sys::Date::new(&(now_ms + 86_400_000.0).into());
    let time = clock_time(ms);
    if day(&date) == day(&today) {
        time
    } else if day(&date) == day(&tomorrow) {
        format!("demain {time}")
    } else {
        format!("{:02}/{:02} {time}", date.get_date(), date.get_month() + 1)
    }
}

/// How far the local time zone is behind UTC at `ms`, in seconds, as
/// `Date.getTimezoneOffset` counts it.
pub fn zone_offset(ms: f64) -> i64 {
    js_sys::Date::new(&ms.into()).get_timezone_offset() as i64 * 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(0), "0 min");
        assert_eq!(duration(27), "27 min");
        assert_eq!(duration(65), "1 h 05");
        assert_eq!(duration(1440 + 190), "1 j 3 h");
    }

    #[test]
    fn numbers_take_a_decimal_comma() {
        assert_eq!(decimal(0.4, 1), "0,4");
        assert_eq!(decimal(29.14, 1), "29,1");
        assert_eq!(degrees(219.6), "220 °C");
    }

    #[test]
    fn only_hex_colours_reach_css() {
        assert_eq!(css_color("0A2989FF").as_deref(), Some("#0A2989"));
        assert_eq!(css_color("9D432C").as_deref(), Some("#9D432C"));
        assert_eq!(css_color("red;}"), None);
        assert_eq!(css_color("0A2989F"), None);
        assert_eq!(swatch(&["FEC600FF".into()]).as_deref(), Some("#FEC600"));
        assert_eq!(
            swatch(&["000000".into(), "FFFFFF".into()]).as_deref(),
            Some("linear-gradient(90deg, #000000 0.0% 50.0%, #FFFFFF 50.0% 100.0%)")
        );
        assert_eq!(swatch(&[]), None);
    }

    #[test]
    fn signals_have_a_word() {
        assert_eq!(wifi_quality(-45), "excellent");
        assert_eq!(wifi_quality(-63), "moyen");
        assert_eq!(wifi_quality(-58), "bon");
        assert_eq!(wifi_quality(-80), "faible");
    }

    #[test]
    fn first_letters_rise() {
        assert_eq!(capitalized("éteindre la lumière"), "Éteindre la lumière");
        assert_eq!(capitalized(""), "");
    }
}
