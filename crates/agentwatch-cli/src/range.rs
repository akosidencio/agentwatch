//! Turning a user's idea of "today" into a UTC range.
//!
//! Events are stored in UTC. Calendar questions are asked in the user's zone.
//! Converting the *boundaries* rather than the *rows* is what keeps a day's
//! total correct without a timezone conversion per row.

use anyhow::{Context as _, Result};
use time::{Date, Duration, OffsetDateTime, UtcOffset};

/// The date format accepted on the command line.
const DATE_FORMAT: &[time::format_description::FormatItem<'_>] =
    time::macros::format_description!("[year]-[month]-[day]");

/// A half-open range of microseconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Range {
    /// Inclusive lower bound.
    pub(crate) from_us: i64,
    /// Exclusive upper bound.
    pub(crate) to_us: i64,
    /// The zone the boundaries and daily buckets are computed in.
    zone: Zone,
    /// How to describe this range to the user.
    pub(crate) label: String,
}

impl Range {
    /// The local calendar date containing one UTC timestamp.
    pub(crate) fn day_label(&self, timestamp_us: i64) -> String {
        let nanos = i128::from(timestamp_us) * 1_000;
        OffsetDateTime::from_unix_timestamp_nanos(nanos).map_or_else(
            |_| "(out of range)".to_owned(),
            |instant| {
                instant
                    .to_offset(self.zone.offset_at(instant))
                    .date()
                    .to_string()
            },
        )
    }
}

/// A timezone whose offset can be resolved at each historical instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Zone {
    /// Used directly for a fixed zone, and when a local lookup fails.
    fallback: UtcOffset,
    /// Whether historical offsets should be read from the operating system.
    local: bool,
}

impl Zone {
    /// A fixed-offset zone, primarily useful for deterministic tests.
    const fn fixed(offset: UtcOffset) -> Self {
        Self {
            fallback: offset,
            local: false,
        }
    }

    /// Resolves the offset effective at one UTC instant.
    fn offset_at(self, instant: OffsetDateTime) -> UtcOffset {
        if self.local {
            UtcOffset::local_offset_at(instant).unwrap_or(self.fallback)
        } else {
            self.fallback
        }
    }

    /// Local midnight on a date, including any historical offset transition.
    fn midnight(self, date: Date) -> OffsetDateTime {
        if self.local {
            resolve_midnight(date, self.fallback, |instant| self.offset_at(instant))
        } else {
            date.midnight().assume_offset(self.fallback)
        }
    }

    /// Today's date in this zone.
    fn today(self) -> Date {
        let now = OffsetDateTime::now_utc();
        now.to_offset(self.offset_at(now)).date()
    }
}

/// Resolves the local timezone, falling back to fixed UTC.
///
/// `time` refuses to read the local zone from a multi-threaded process because
/// the underlying C call is not thread safe. The CLI is single threaded, so
/// this normally succeeds; when it does not, UTC is the honest answer and the
/// caller says so rather than silently shifting the user's days.
pub(crate) fn local_zone() -> (Zone, bool) {
    UtcOffset::current_local_offset().map_or((Zone::fixed(UtcOffset::UTC), false), |offset| {
        (
            Zone {
                fallback: offset,
                local: true,
            },
            true,
        )
    })
}

/// Builds the range for the last `days` calendar days, ending today.
pub(crate) fn last_days(days: u32, zone: Zone) -> Range {
    let today = zone.today();
    let start = today - Duration::days(i64::from(days.saturating_sub(1)));
    let label = if days == 1 {
        "today".to_owned()
    } else {
        format!("last {days} days")
    };
    between(start, today, zone, label)
}

/// Parses an explicit `--from` / `--to` pair of local dates.
///
/// # Errors
///
/// Returns an error if either date is unparseable or the range is inverted.
pub(crate) fn explicit(from: &str, to: &str, zone: Zone) -> Result<Range> {
    let start = Date::parse(from, DATE_FORMAT)
        .with_context(|| format!("`{from}` is not a date; expected YYYY-MM-DD"))?;
    let end = Date::parse(to, DATE_FORMAT)
        .with_context(|| format!("`{to}` is not a date; expected YYYY-MM-DD"))?;

    anyhow::ensure!(start <= end, "--from {from} is after --to {to}");
    Ok(between(start, end, zone, format!("{from} to {to}")))
}

/// A range covering everything ever recorded.
pub(crate) fn all_time(zone: Zone) -> Range {
    Range {
        from_us: 0,
        to_us: i64::MAX,
        zone,
        label: "all time".to_owned(),
    }
}

/// Builds a range from local midnight on `start` to local midnight after `end`.
fn between(start: Date, end: Date, zone: Zone, label: String) -> Range {
    let from = zone.midnight(start);
    let to = zone.midnight(end + Duration::days(1));

    Range {
        from_us: micros(from),
        to_us: micros(to),
        zone,
        label,
    }
}

/// Finds the UTC instant corresponding to local midnight.
///
/// The first offset is normally today's. Looking it up at that provisional
/// instant yields the historical offset; a second pass then lands on the exact
/// midnight. Iterating also handles transitions close to midnight without
/// assuming that a zone always changes at 02:00.
fn resolve_midnight(
    date: Date,
    initial: UtcOffset,
    mut offset_at: impl FnMut(OffsetDateTime) -> UtcOffset,
) -> OffsetDateTime {
    let midnight = date.midnight();
    let mut offset = initial;
    for _ in 0..4 {
        let instant = midnight.assume_offset(offset);
        let resolved = offset_at(instant);
        if resolved == offset {
            return instant;
        }
        offset = resolved;
    }
    midnight.assume_offset(offset)
}

/// Converts to microseconds since the Unix epoch, saturating.
fn micros(value: OffsetDateTime) -> i64 {
    i64::try_from(value.unix_timestamp_nanos() / 1_000).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asia/Manila, the zone this was developed in.
    fn manila() -> UtcOffset {
        UtcOffset::from_hms(8, 0, 0).expect("valid offset")
    }

    #[test]
    fn an_explicit_range_spans_whole_local_days() {
        let range = explicit("2026-08-01", "2026-08-01", Zone::fixed(manila())).expect("parses");

        // 2026-08-01T00:00+08 is 2026-07-31T16:00Z, and the range is one day long.
        assert_eq!(range.to_us - range.from_us, 86_400 * 1_000_000);
    }

    #[test]
    fn the_same_dates_differ_between_zones() {
        let manila_range =
            explicit("2026-08-01", "2026-08-01", Zone::fixed(manila())).expect("parses");
        let utc_range =
            explicit("2026-08-01", "2026-08-01", Zone::fixed(UtcOffset::UTC)).expect("parses");

        assert_eq!(
            utc_range.from_us - manila_range.from_us,
            8 * 3600 * 1_000_000,
            "a Manila day starts eight hours before the UTC day of the same name"
        );
    }

    #[test]
    fn an_inverted_range_is_rejected() {
        assert!(explicit("2026-08-21", "2026-08-01", Zone::fixed(manila())).is_err());
    }

    #[test]
    fn an_unparseable_date_names_the_expected_format() {
        let error =
            explicit("yesterday", "2026-08-01", Zone::fixed(manila())).expect_err("rejected");
        assert!(format!("{error}").contains("YYYY-MM-DD"));
    }

    #[test]
    fn one_day_is_labelled_today() {
        assert_eq!(last_days(1, Zone::fixed(manila())).label, "today");
    }

    #[test]
    fn seven_days_spans_seven_whole_days() {
        let range = last_days(7, Zone::fixed(manila()));
        assert_eq!(range.to_us - range.from_us, 7 * 86_400 * 1_000_000);
    }

    #[test]
    fn all_time_starts_at_the_epoch() {
        assert_eq!(all_time(Zone::fixed(manila())).from_us, 0);
    }

    #[test]
    fn historical_midnight_does_not_reuse_the_current_offset() {
        let summer = UtcOffset::from_hms(-4, 0, 0).expect("summer offset");
        let winter = UtcOffset::from_hms(-5, 0, 0).expect("winter offset");
        let january = Date::from_calendar_date(2026, time::Month::January, 15).expect("date");

        let midnight = resolve_midnight(january, summer, |_| winter);

        assert_eq!(midnight.offset(), winter);
        assert_eq!(midnight.hour(), 0);
    }
}
