//! v1 的本地时间文本解析。v1 不带时区，这里按"本地时间的秒数"处理，只关心相对差值。

pub const SECONDS_PER_DAY: i64 = 86_400;

/// 公历日期 → 1970-01-01 起的天数（Howard Hinnant 的 days_from_civil）。
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 1970-01-01 起的天数 → (年, 月, 日)。
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (month <= 2) as i64, month, day)
}

pub fn format_date(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// "yyyy-MM-dd" → 天数。
pub fn parse_date(text: &str) -> Option<i64> {
    let mut parts = text.trim().splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| days_from_civil(y, m, d))
}

/// "yyyy-MM-dd HH:mm:ss"（也接受 T 分隔、省略秒、带小数秒）→ 秒。
pub fn parse_datetime(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, time) = text.split_once([' ', 'T']).unwrap_or((text, "00:00:00"));
    let days = parse_date(date)?;
    let mut parts = time.split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next().map_or(Some(0.0), |s| s.parse().ok())?;
    if parts.next().is_some()
        || !(0..24).contains(&h)
        || !(0..60).contains(&m)
        || !(0.0..61.0).contains(&s)
    {
        return None;
    }
    Some(days * SECONDS_PER_DAY + h * 3600 + m * 60 + s.floor() as i64)
}

/// ISO-8601 时长（v1 的 `Duration`，例如 PT1H30M、P1DT2H、PT45.5S）→ 秒（四舍五入）。
pub fn parse_iso_duration(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, text) = text
        .strip_prefix('-')
        .map_or((false, text), |rest| (true, rest));
    let rest = text.strip_prefix(['P', 'p'])?;
    let mut total = 0.0f64;
    let mut in_time = false;
    let mut number = String::new();
    let mut any = false;
    for c in rest.chars() {
        match c {
            'T' | 't' if number.is_empty() => in_time = true,
            '0'..='9' | '.' | '-' => number.push(c),
            ',' => number.push('.'),
            _ => {
                let value: f64 = number.parse().ok()?;
                number.clear();
                any = true;
                total += value
                    * match (c.to_ascii_uppercase(), in_time) {
                        ('W', false) => 604_800.0,
                        ('D', false) => 86_400.0,
                        ('H', true) => 3_600.0,
                        ('M', true) => 60.0,
                        ('S', true) => 1.0,
                        _ => return None,
                    };
            }
        }
    }
    if !number.is_empty() || !any {
        return None;
    }
    let seconds = total.round() as i64;
    Some(if negative { -seconds } else { seconds })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for days in [-1_000_000, -1, 0, 1, 19_000, 20_000, 2_000_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(parse_date("1970-01-01"), Some(0));
        assert_eq!(
            format_date(parse_date("2024-12-31").unwrap() + 1),
            "2025-01-01"
        );
    }

    #[test]
    fn datetimes() {
        let base = parse_date("2025-06-21").unwrap() * SECONDS_PER_DAY;
        assert_eq!(parse_datetime("2025-06-21 07:00:00"), Some(base + 25_200));
        assert_eq!(parse_datetime("2025-06-21T07:00"), Some(base + 25_200));
        assert_eq!(
            parse_datetime("2025-06-21 23:59:59.900"),
            Some(base + 86_399)
        );
        assert_eq!(parse_datetime("2025-06-21 25:00:00"), None);
    }

    #[test]
    fn iso_durations() {
        assert_eq!(parse_iso_duration("PT1H"), Some(3_600));
        assert_eq!(parse_iso_duration("PT56M"), Some(3_360));
        assert_eq!(parse_iso_duration("PT1H30M15S"), Some(5_415));
        assert_eq!(parse_iso_duration("P1DT2H"), Some(93_600));
        assert_eq!(parse_iso_duration("PT0.5S"), Some(1));
        assert_eq!(parse_iso_duration("PT"), None);
        assert_eq!(parse_iso_duration("1H"), None);
    }
}
