//! Number formatting shared by every surface.
//!
//! Lives here rather than in one of the front-ends because the CLI, the live
//! view, and the menu bar all render the same counts, and two copies of a
//! formatter is two chances for the same number to be shown differently in two
//! places.

/// Formats a count with thousands separators.
#[must_use]
pub fn thousands(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }

    if value < 0 { format!("-{out}") } else { out }
}

/// Formats a count short enough for a menu bar.
///
/// Loses precision on purpose: the menu bar strip is shared with every other
/// status item, and `6.0B` earns its place where `6,006,377,004` does not.
///
/// Promotion is decided on the *rounded* value, not the raw one. Testing the
/// raw value renders 999,999 as `1000.0K` — both the wrong unit and two
/// characters wider than the format is supposed to allow.
#[must_use]
pub fn compact(value: i64) -> String {
    if value < 0 {
        // `-i64::MIN` overflows, so the digits are taken unsigned.
        return format!("-{}", compact_unsigned(value.unsigned_abs()));
    }

    #[expect(clippy::cast_sign_loss, reason = "negatives are handled above")]
    compact_unsigned(value as u64)
}

/// Formats a non-negative count.
fn compact_unsigned(value: u64) -> String {
    /// Scale and suffix, largest first.
    const UNITS: [(f64, &str); 4] = [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")];

    /// Smallest scaled value that still rounds up to `1.0` at one decimal.
    const ROUNDS_TO_ONE: f64 = 0.999_5;

    #[expect(
        clippy::cast_precision_loss,
        reason = "display only, one decimal place"
    )]
    let as_float = value as f64;

    for (scale, suffix) in UNITS {
        let scaled = as_float / scale;
        if scaled >= ROUNDS_TO_ONE {
            return format!("{scaled:.1}{suffix}");
        }
    }

    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_separates_at_every_third_digit() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(6_006_377_004), "6,006,377,004");
    }

    #[test]
    fn thousands_handles_negatives() {
        assert_eq!(thousands(-1_234), "-1,234");
    }

    #[test]
    fn thousands_handles_the_smallest_integer() {
        // `-i64::MIN` overflows, so negation must not be how the sign is taken.
        assert_eq!(thousands(i64::MIN), "-9,223,372,036,854,775,808");
    }

    #[test]
    fn compact_covers_every_magnitude() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1_500), "1.5K");
        assert_eq!(compact(2_400_000), "2.4M");
        assert_eq!(compact(6_000_000_000), "6.0B");
    }

    #[test]
    fn compact_promotes_on_the_rounded_value_not_the_raw_one() {
        // 999_999 / 1_000 rounds to 1000.0, which is both the wrong unit and
        // wider than a menu bar title should ever be.
        assert_eq!(compact(999_999), "1.0M");
        assert_eq!(compact(999_999_999), "1.0B");
        assert_eq!(compact(999), "999");
    }

    #[test]
    fn compact_stays_short_across_realistic_magnitudes() {
        for value in [
            0,
            1,
            999,
            1_000,
            999_999,
            1_000_000,
            6_006_377_004,
            999_999_999_999,
        ] {
            assert!(
                compact(value).len() <= 6,
                "{value} rendered as {}",
                compact(value)
            );
        }
    }

    #[test]
    fn compact_does_not_panic_at_the_extremes() {
        let _ = compact(i64::MAX);
        let _ = compact(i64::MIN);
    }

    #[test]
    fn compact_handles_negatives() {
        assert_eq!(compact(-1_500), "-1.5K");
    }
}
