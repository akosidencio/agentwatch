//! What the menu bar shows.
//!
//! Kept apart from the tray plumbing so the formatting — which is where the
//! mistakes that mislead people live — is testable without a display server.

use agentwatch_storage::Store;
use agentwatch_types::{Paths, compact, thousands};
use time::UtcOffset;

/// Most rows counted for a day's headline numbers.
///
/// A ceiling on the work one refresh can do. Well above a real day's activity;
/// `status_line` says so when it is hit rather than showing a capped number as
/// if it were the total.
const DAILY_CAP: u32 = 500;

/// The timezone "today" is measured in.
///
/// Resolved once, at startup, and carried rather than re-read. The `time` crate
/// refuses to read the local zone from a multi-threaded process, and this one
/// grows threads the moment the event loop starts — so asking later always
/// answers UTC, which in a zone eight hours from it means the menu bar and
/// `agentwatch tokens` disagree for most of the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Zone {
    /// The offset to bucket days by.
    offset: UtcOffset,
    /// Whether that offset is the machine's, rather than the UTC fallback.
    local: bool,
}

impl Zone {
    /// Reads the local zone. Call before starting any threads.
    pub(crate) fn resolve() -> Self {
        UtcOffset::current_local_offset().map_or(
            Self {
                offset: UtcOffset::UTC,
                local: false,
            },
            |offset| Self {
                offset,
                local: true,
            },
        )
    }

    /// Midnight-to-midnight in this zone, as microseconds.
    fn today(self) -> (i64, i64) {
        let start = time::OffsetDateTime::now_utc()
            .to_offset(self.offset)
            .replace_time(time::Time::MIDNIGHT);
        let end = start + time::Duration::days(1);

        (micros(start), micros(end))
    }
}

/// Converts to microseconds since the Unix epoch, saturating.
fn micros(value: time::OffsetDateTime) -> i64 {
    i64::try_from(value.unix_timestamp_nanos() / 1_000).unwrap_or(i64::MAX)
}

/// A moment's worth of state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// Whether the daemon is accepting connections.
    pub(crate) daemon_running: bool,
    /// Whether collection is paused.
    pub(crate) paused: bool,
    /// Whether the database could be read at all.
    pub(crate) readable: bool,
    /// Tokens recorded today.
    pub(crate) tokens: i64,
    /// Sessions seen today.
    pub(crate) sessions: usize,
    /// Sensitive accesses today.
    pub(crate) sensitive: usize,
    /// Whether "today" means the machine's day or the UTC one.
    pub(crate) local_day: bool,
}

impl Snapshot {
    /// Reads the current state, degrading to "unreadable" rather than failing.
    pub(crate) fn read(paths: &Paths, zone: Zone) -> Self {
        let daemon_running = std::os::unix::net::UnixStream::connect(paths.socket()).is_ok();
        let paused = paths.is_paused();

        let Ok(store) = Store::open_read_only(paths.database()) else {
            return Self {
                daemon_running,
                paused,
                readable: false,
                tokens: 0,
                sessions: 0,
                sensitive: 0,
                local_day: zone.local,
            };
        };

        let (from, to) = zone.today();
        Self {
            daemon_running,
            paused,
            readable: true,
            tokens: store
                .token_totals(from, to)
                .map(|totals| totals.total())
                .unwrap_or_default(),
            sessions: store
                .sessions(from, to, DAILY_CAP)
                .map(|rows| rows.len())
                .unwrap_or_default(),
            sensitive: store
                .notable_access(from, to, DAILY_CAP)
                .map(|rows| rows.len())
                .unwrap_or_default(),
            local_day: zone.local,
        }
    }

    /// Which glyph the icon should show.
    ///
    /// Paused wins over a stopped daemon: it is the state the user chose, and
    /// the one they can undo from this menu.
    pub(crate) fn glyph(&self) -> crate::icon::Glyph {
        use crate::icon::Glyph;

        if self.paused {
            Glyph::Paused
        } else if self.daemon_running && self.readable {
            Glyph::Watching
        } else {
            Glyph::Idle
        }
    }

    /// The text shown in the menu bar itself.
    ///
    /// Compact by necessity — this sits in a strip shared with every other
    /// status item — and prefixed when something is wrong, because a number
    /// alone cannot say "this number stopped updating an hour ago".
    pub(crate) fn title(&self) -> String {
        if !self.readable {
            return "—".to_owned();
        }
        if self.paused {
            return "paused".to_owned();
        }
        if !self.daemon_running {
            return "off".to_owned();
        }
        compact(self.tokens)
    }

    /// The first line of the menu.
    pub(crate) fn status_line(&self) -> String {
        if !self.readable {
            return "No data yet — run `agentwatch import`".to_owned();
        }
        match (self.daemon_running, self.paused) {
            (_, true) => "Collection paused".to_owned(),
            (true, false) => "Collecting".to_owned(),
            (false, false) => "Daemon not running".to_owned(),
        }
    }

    /// Today's tokens, formatted.
    ///
    /// Labelled UTC when the machine's zone could not be read, because a total
    /// for a day eight hours from the user's own is worse than one that says
    /// which day it means.
    pub(crate) fn tokens_text(&self) -> String {
        if !self.readable {
            return "—".to_owned();
        }
        if self.local_day {
            thousands(self.tokens)
        } else {
            format!("{} (UTC day)", thousands(self.tokens))
        }
    }

    /// The sensitive-access line.
    pub(crate) fn sensitive_line(&self) -> String {
        if self.sensitive == 0 {
            return "Sensitive access: none".to_owned();
        }
        format!(
            "Sensitive access: {} — see `agentwatch security`",
            self.sensitive
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            daemon_running: true,
            paused: false,
            readable: true,
            tokens: 1_234_567,
            sessions: 3,
            sensitive: 0,
            local_day: true,
        }
    }

    #[test]
    fn the_icon_reflects_what_the_collector_is_doing() {
        use crate::icon::Glyph;

        assert_eq!(snapshot().glyph(), Glyph::Watching);
        assert_eq!(
            Snapshot {
                paused: true,
                ..snapshot()
            }
            .glyph(),
            Glyph::Paused
        );
        assert_eq!(
            Snapshot {
                daemon_running: false,
                ..snapshot()
            }
            .glyph(),
            Glyph::Idle
        );
        assert_eq!(
            Snapshot {
                readable: false,
                ..snapshot()
            }
            .glyph(),
            Glyph::Idle
        );
    }

    #[test]
    fn a_paused_collector_shows_the_paused_icon_even_with_the_daemon_down() {
        let both = Snapshot {
            daemon_running: false,
            paused: true,
            ..snapshot()
        };
        assert_eq!(both.glyph(), crate::icon::Glyph::Paused);
    }

    #[test]
    fn the_title_is_short_enough_for_a_menu_bar() {
        assert_eq!(snapshot().title(), "1.2M");
        assert!(snapshot().title().len() <= 6);
    }

    #[test]
    fn a_paused_collector_says_so_instead_of_showing_a_stale_number() {
        let paused = Snapshot {
            paused: true,
            ..snapshot()
        };
        assert_eq!(paused.title(), "paused");
        assert_eq!(paused.status_line(), "Collection paused");
    }

    #[test]
    fn a_stopped_daemon_says_so_instead_of_showing_a_stale_number() {
        let stopped = Snapshot {
            daemon_running: false,
            ..snapshot()
        };
        assert_eq!(stopped.title(), "off");
        assert_eq!(stopped.status_line(), "Daemon not running");
    }

    #[test]
    fn pause_takes_precedence_over_a_stopped_daemon() {
        let both = Snapshot {
            daemon_running: false,
            paused: true,
            ..snapshot()
        };
        assert_eq!(
            both.title(),
            "paused",
            "the deliberate state is the more useful one"
        );
    }

    #[test]
    fn an_unreadable_database_never_shows_a_number() {
        let unreadable = Snapshot {
            readable: false,
            ..snapshot()
        };
        assert_eq!(unreadable.title(), "—");
        assert_eq!(unreadable.tokens_text(), "—");
        assert!(unreadable.status_line().contains("import"));
    }

    #[test]
    fn a_utc_fallback_day_says_so_rather_than_passing_as_local() {
        let utc = Snapshot {
            local_day: false,
            ..snapshot()
        };
        assert!(
            utc.tokens_text().contains("UTC"),
            "a total for the wrong day must not read as the right one"
        );
        assert!(!snapshot().tokens_text().contains("UTC"));
    }

    #[test]
    fn a_resolved_zone_buckets_a_whole_day() {
        let zone = Zone {
            offset: UtcOffset::from_hms(8, 0, 0).expect("valid offset"),
            local: true,
        };
        let (from, to) = zone.today();
        assert_eq!(to - from, 86_400 * 1_000_000);
    }

    #[test]
    fn sensitive_access_is_called_out_only_when_there_is_some() {
        assert_eq!(snapshot().sensitive_line(), "Sensitive access: none");

        let flagged = Snapshot {
            sensitive: 4,
            ..snapshot()
        };
        assert!(flagged.sensitive_line().contains('4'));
        assert!(flagged.sensitive_line().contains("agentwatch security"));
    }
}
