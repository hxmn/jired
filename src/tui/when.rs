//! Timestamps in the words and the widths a screen needs them in.
//!
//! Two renderings of the same thing, because two places want it differently.
//! A form field has a whole row to explain itself, so it says "4 hours ago" —
//! nobody reads an ISO timestamp by eye, and the point of showing the start
//! time at all is that it can be checked. A table column has six cells to work
//! with and repeats down the screen, so it says `4h`: the unit is what the
//! reader is comparing, and spelling it out would push a real column off the
//! right-hand edge.

use chrono::{DateTime, Utc};

/// How long ago `when` was, in the words someone would use for it.
pub fn ago(now: DateTime<Utc>, when: DateTime<Utc>) -> String {
    let minutes = (now - when).num_minutes();
    match minutes {
        // A start time in the future is a mistake worth naming rather than
        // describing as "in -3 hours".
        m if m < -1 => "in the future".to_string(),
        m if m < 5 => "just now".to_string(),
        m if m < 60 => format!("{m} minutes ago"),
        m if m < 120 => "an hour ago".to_string(),
        m if m < 60 * 24 => format!("{} hours ago", m / 60),
        m if m < 60 * 48 => "yesterday".to_string(),
        m => format!("{} days ago", m / (60 * 24)),
    }
}

/// The same age in a table cell: at most five columns, one unit, no words.
///
/// The unit steps up as soon as the number would need three digits, so the
/// column stays narrow without ever rounding a week down to a day.
pub fn short(now: DateTime<Utc>, when: DateTime<Utc>) -> String {
    let minutes = (now - when).num_minutes();
    match minutes {
        m if m < -1 => "ahead".to_string(),
        m if m < 1 => "now".to_string(),
        m if m < 60 => format!("{m}m"),
        m if m < 60 * 24 => format!("{}h", m / 60),
        m if m < 60 * 24 * 14 => format!("{}d", m / (60 * 24)),
        m if m < 60 * 24 * 70 => format!("{}w", m / (60 * 24 * 7)),
        m if m < 60 * 24 * 365 => format!("{}mo", m / (60 * 24 * 30)),
        m => format!("{}y", m / (60 * 24 * 365)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        crate::api::model::parse_timestamp("2026-08-21T13:00:00Z").expect("a fixed clock")
    }

    #[test]
    fn words_describe_the_age_the_way_a_person_would() {
        let now = now();
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now, now - Duration::minutes(20)), "20 minutes ago");
        assert_eq!(ago(now, now - Duration::hours(1)), "an hour ago");
        assert_eq!(ago(now, now - Duration::hours(5)), "5 hours ago");
        assert_eq!(ago(now, now - Duration::days(1)), "yesterday");
        assert_eq!(ago(now, now - Duration::days(3)), "3 days ago");
        assert_eq!(ago(now, now + Duration::hours(3)), "in the future");
    }

    /// Every cell has to fit the column the table reserves for it.
    #[test]
    fn the_short_form_never_outgrows_its_column() {
        let now = now();
        for (delta, expected) in [
            (Duration::zero(), "now"),
            (Duration::minutes(20), "20m"),
            (Duration::hours(5), "5h"),
            (Duration::days(3), "3d"),
            (Duration::days(13), "13d"),
            (Duration::days(20), "2w"),
            (Duration::days(69), "9w"),
            (Duration::days(100), "3mo"),
            (Duration::days(400), "1y"),
        ] {
            let cell = short(now, now - delta);
            assert_eq!(cell, expected, "{delta} ago");
            assert!(cell.chars().count() <= 5, "`{cell}` is too wide");
        }
        assert_eq!(short(now, now + Duration::hours(3)), "ahead");
    }
}
