use jiff::{Timestamp, ToSpan, Zoned};

pub fn epoch_seconds_from_f64(raw: f64) -> Option<i64> {
    raw.is_finite()
        .then(|| normalize_epoch_seconds(raw.round() as i64))
}

pub fn normalize_epoch_seconds(raw: i64) -> i64 {
    if raw.unsigned_abs() >= 10_000_000_000 {
        raw / 1_000
    } else {
        raw
    }
}

pub fn reset_epoch_seconds_from_str(raw: &str, reference_epoch: Option<i64>) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<i64>()
        .ok()
        .map(normalize_epoch_seconds)
        .or_else(|| raw.parse::<f64>().ok().and_then(epoch_seconds_from_f64))
        .or_else(|| {
            raw.parse::<Timestamp>()
                .ok()
                .map(|timestamp| timestamp.as_second())
        })
        .or_else(|| {
            reference_epoch
                .and_then(|reference_epoch| human_reset_epoch_seconds(raw, reference_epoch))
        })
}

fn human_reset_epoch_seconds(raw: &str, reference_epoch: i64) -> Option<i64> {
    let (body, time_zone) = split_parenthesized_time_zone(raw)?;
    let reference = Timestamp::from_second(reference_epoch)
        .ok()?
        .in_tz(time_zone)
        .ok()?;
    if body.contains(',') {
        month_day_reset_epoch(body, time_zone, reference_epoch, reference.year())
    } else {
        time_only_reset_epoch(body, time_zone, reference_epoch, &reference)
    }
}

fn split_parenthesized_time_zone(raw: &str) -> Option<(&str, &str)> {
    let without_close = raw.trim().strip_suffix(')')?;
    let open = without_close.rfind('(')?;
    let body = without_close[..open].trim();
    let time_zone = without_close[open + 1..].trim();
    if body.is_empty() || time_zone.is_empty() {
        None
    } else {
        Some((body, time_zone))
    }
}

fn month_day_reset_epoch(
    body: &str,
    time_zone: &str,
    reference_epoch: i64,
    reference_year: i16,
) -> Option<i64> {
    let epoch = month_day_reset_epoch_for_year(body, time_zone, reference_year)?;
    if epoch > reference_epoch {
        return Some(epoch);
    }
    month_day_reset_epoch_for_year(body, time_zone, reference_year.checked_add(1)?)
}

fn month_day_reset_epoch_for_year(body: &str, time_zone: &str, year: i16) -> Option<i64> {
    let (date_part, time_part) = body.split_once(',')?;
    let input = format!(
        "{}, {} {} {}",
        date_part.trim(),
        year,
        time_part.trim(),
        time_zone
    );
    parse_zoned_epoch(
        &input,
        &[
            "%b %d, %Y %-I:%M%P %Q",
            "%b %d, %Y %-I%P %Q",
            "%B %d, %Y %-I:%M%P %Q",
            "%B %d, %Y %-I%P %Q",
            "%b %d, %Y %I:%M%P %Q",
            "%b %d, %Y %I%P %Q",
            "%B %d, %Y %I:%M%P %Q",
            "%B %d, %Y %I%P %Q",
        ],
    )
}

fn time_only_reset_epoch(
    body: &str,
    time_zone: &str,
    reference_epoch: i64,
    reference: &Zoned,
) -> Option<i64> {
    let epoch = time_only_reset_epoch_for_date(body, time_zone, &reference.date().to_string())?;
    if epoch > reference_epoch {
        return Some(epoch);
    }
    let next_date = reference.date().checked_add(1.day()).ok()?;
    time_only_reset_epoch_for_date(body, time_zone, &next_date.to_string())
}

fn time_only_reset_epoch_for_date(body: &str, time_zone: &str, date: &str) -> Option<i64> {
    let input = format!("{} {} {}", date, body.trim(), time_zone);
    parse_zoned_epoch(
        &input,
        &[
            "%F %-I:%M%P %Q",
            "%F %-I%P %Q",
            "%F %I:%M%P %Q",
            "%F %I%P %Q",
        ],
    )
}

fn parse_zoned_epoch(input: &str, formats: &[&str]) -> Option<i64> {
    formats.iter().find_map(|format| {
        Zoned::strptime(format, input)
            .ok()
            .map(|zoned| zoned.timestamp().as_second())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_epoch_seconds_from_str_accepts_numeric_and_rfc3339() {
        assert_eq!(
            reset_epoch_seconds_from_str("1783668000", None),
            Some(1_783_668_000)
        );
        assert_eq!(
            reset_epoch_seconds_from_str("1783668000000", None),
            Some(1_783_668_000)
        );
        assert_eq!(
            reset_epoch_seconds_from_str("1783668000.0", None),
            Some(1_783_668_000)
        );
        assert_eq!(
            reset_epoch_seconds_from_str("2026-07-10T07:20:00Z", None),
            Some(1_783_668_000)
        );
    }

    #[test]
    fn reset_epoch_seconds_from_str_parses_human_time_with_reference() {
        let reference_epoch = 1_783_666_800;
        assert_eq!(
            reset_epoch_seconds_from_str("3:20pm(Asia/Taipei)", Some(reference_epoch)),
            Some(1_783_668_000)
        );
        assert_eq!(
            reset_epoch_seconds_from_str("Jul 12, 9pm (Asia/Taipei)", Some(reference_epoch)),
            Some(1_783_861_200)
        );
    }

    #[test]
    fn reset_epoch_seconds_from_str_rolls_human_times_forward() {
        let reference_epoch = 1_783_670_400;
        assert_eq!(
            reset_epoch_seconds_from_str("3:20pm(Asia/Taipei)", Some(reference_epoch)),
            Some(1_783_754_400)
        );
        assert_eq!(
            reset_epoch_seconds_from_str("Jul 10, 3pm (Asia/Taipei)", Some(reference_epoch)),
            Some(1_815_202_800)
        );
    }

    #[test]
    fn reset_epoch_seconds_from_str_rejects_unparseable_human_times() {
        assert_eq!(reset_epoch_seconds_from_str("", Some(1_783_666_800)), None);
        assert_eq!(
            reset_epoch_seconds_from_str("3:20pm(Not/AZone)", Some(1_783_666_800)),
            None
        );
        assert_eq!(
            reset_epoch_seconds_from_str("3:20pm(Asia/Taipei)", None),
            None
        );
    }
}
