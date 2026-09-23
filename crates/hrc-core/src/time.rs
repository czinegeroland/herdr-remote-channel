//! Timestamps in the one shape this project writes, and short durations.
//!
//! Until `docs/REFACTOR.md` R3 this arithmetic existed three times: once in
//! the CLI to resolve expiries, once in the inbox side view to print ages,
//! and a third duration formatter in the sidebar. Two of them agreed; the
//! third checked month and day ranges and the others did not. Timestamps are
//! compared as strings across this codebase, so every copy that parses one
//! has to agree on what one is.
//!
//! No date-time crate, deliberately. The only shape accepted is the fixed
//! `YYYY-MM-DDTHH:MM:SSZ` that `Database::utc_now` writes; accepting offsets
//! and fractions nothing here produces would mean quietly mis-parsing one,
//! and the arithmetic needed is Howard Hinnant's two civil-date functions.

/// Seconds since the Unix epoch for one `YYYY-MM-DDTHH:MM:SSZ` timestamp.
///
/// Anything else is refused, including an impossible month or day. A
/// timestamp this project wrote never has one, so refusing costs nothing,
/// and parsing month 13 into arithmetic would produce a plausible wrong date
/// rather than an error.
pub fn epoch_seconds(timestamp: &str) -> Option<i64> {
    let bytes = timestamp.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[10] != b'T' || bytes[19] != b'Z' {
        return None;
    }

    let field = |from: usize, to: usize| timestamp.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Renders seconds since the Unix epoch in the same fixed shape.
pub fn rfc3339_from(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let remainder = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remainder / 3600,
        (remainder % 3600) / 60,
        remainder % 60
    )
}

/// A duration short enough for a border or a column: `12s`, `3m`, `2h`, `4d`.
///
/// Deliberately coarse. The exact age of a row or of the last fetch is not a
/// decision input; whether it was seconds or days ago is. Callers add their
/// own word (`ago`, `synced`), which is the only way the three copies this
/// replaced differed.
pub fn short_duration(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = month_index + if month_index < 10 { 3 } else { -9 };

    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests;
