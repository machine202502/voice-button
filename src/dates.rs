//! Which slice of time a list shows.
//!
//! One vocabulary for every list that can be narrowed by date — the history, the
//! Telegram log and the phone's monologs — so the buttons read the same
//! everywhere: today, yesterday, this week, a period, all time.
//!
//! A filter is expressed as two bounds in UTC, because that is what the stored
//! timestamps are. The day boundaries are local: "today" starts at the user's
//! midnight, not at UTC midnight.

use chrono::NaiveDate;

/// The named spans, plus the free one.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Preset {
    #[default]
    Today,
    Yesterday,
    /// Seven calendar days including today.
    Week,
    /// Two dates the person picked in the calendar.
    Period,
    All,
}

/// A preset, and for [`Preset::Period`] the two days it covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DateFilter {
    pub preset: Preset,
    /// First day of the period, inclusive. Only read when `preset` is `Period`.
    pub from: NaiveDate,
    /// Last day of the period, inclusive. Only read when `preset` is `Period`.
    pub to: NaiveDate,
}

impl Default for DateFilter {
    fn default() -> Self {
        Self::preset(Preset::Today)
    }
}

impl DateFilter {
    /// A named span, with the period left pointing at today.
    pub fn preset(preset: Preset) -> Self {
        let today = chrono::Local::now().date_naive();
        Self {
            preset,
            from: today,
            to: today,
        }
    }

    pub fn everything() -> Self {
        Self::preset(Preset::All)
    }

    pub fn today() -> Self {
        Self::preset(Preset::Today)
    }

    pub fn yesterday() -> Self {
        Self::preset(Preset::Yesterday)
    }

    pub fn week() -> Self {
        Self::preset(Preset::Week)
    }

    /// Every button, in the order they are drawn.
    pub fn presets() -> [Preset; 5] {
        [
            Preset::Today,
            Preset::Yesterday,
            Preset::Week,
            Preset::Period,
            Preset::All,
        ]
    }

    /// The English key the interface translates.
    pub fn label(self) -> &'static str {
        self.preset.label()
    }

    /// True when the two days of the period are in order; the picker fixes it up.
    pub fn period_is_ordered(self) -> bool {
        self.from <= self.to
    }

    /// Lower bound in UTC, or `""` for "no lower bound".
    pub fn since(self) -> String {
        match self.preset {
            Preset::All => String::new(),
            Preset::Today => midnight_offset(0),
            Preset::Yesterday => midnight_offset(1),
            Preset::Week => midnight_offset(6),
            Preset::Period => midnight(self.from),
        }
    }

    /// Upper bound in UTC, or `""` for "no upper bound".
    pub fn until(self) -> String {
        match self.preset {
            Preset::Yesterday => midnight_offset(0),
            Preset::Period => midnight(self.to + chrono::Days::new(1)),
            _ => String::new(),
        }
    }

    /// The SQL condition for a quoted statement, taking `?1` and `?2`.
    ///
    /// `column` is the timestamp to compare; `fallback` is used when that column
    /// is empty, which is how rows written before the column existed still count
    /// by their own date. `datetime()` is applied to both sides so a value stored
    /// with a local offset and a bound stored in UTC compare as times, not text.
    pub fn clause(column: &str, fallback: &str) -> String {
        let value = format!("datetime(COALESCE(NULLIF({column}, ''), {fallback}))");
        format!("(?1 = '' OR {value} >= datetime(?1)) AND (?2 = '' OR {value} < datetime(?2))")
    }

    /// The same condition with anonymous parameters, for a statement that is
    /// built from a list of clauses rather than written out.
    pub fn clause_anonymous(column: &str, fallback: &str) -> String {
        let value = format!("datetime(COALESCE(NULLIF({column}, ''), {fallback}))");
        format!("(? = '' OR {value} >= datetime(?)) AND (? = '' OR {value} < datetime(?))")
    }
}

impl Preset {
    pub fn label(self) -> &'static str {
        match self {
            Preset::Today => "Today",
            Preset::Yesterday => "Yesterday",
            Preset::Week => "Week",
            Preset::Period => "Period",
            Preset::All => "All time",
        }
    }
}

/// Midnight `days` days ago, in UTC.
fn midnight_offset(days: u64) -> String {
    midnight(chrono::Local::now().date_naive() - chrono::Days::new(days))
}

/// Midnight of a local day, in UTC. An empty string means "no bound".
fn midnight(day: NaiveDate) -> String {
    let Some(naive) = day.and_hms_opt(0, 0, 0) else {
        return String::new();
    };
    match chrono::TimeZone::from_local_datetime(&chrono::Local, &naive).earliest() {
        Some(moment) => moment
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_has_a_label() {
        for preset in DateFilter::presets() {
            assert!(!preset.label().is_empty());
        }
        assert_eq!(DateFilter::default().preset, Preset::Today);
    }

    #[test]
    fn today_starts_at_midnight_and_has_no_end() {
        let today = DateFilter::preset(Preset::Today);
        assert!(today.since().ends_with('Z'));
        assert_eq!(today.until(), "");
        assert!(today.since() < chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }

    #[test]
    fn yesterday_is_a_window_that_ends_where_today_starts() {
        let yesterday = DateFilter::preset(Preset::Yesterday);
        let today = DateFilter::preset(Preset::Today);
        assert!(!yesterday.since().is_empty());
        assert_eq!(yesterday.until(), today.since(), "the windows meet exactly");
        assert!(yesterday.since() < yesterday.until());
    }

    #[test]
    fn the_week_reaches_further_back_than_yesterday() {
        assert!(
            DateFilter::preset(Preset::Week).since() < DateFilter::preset(Preset::Yesterday).since()
        );
        assert_eq!(DateFilter::preset(Preset::Week).until(), "");
    }

    #[test]
    fn all_time_has_no_bounds() {
        let all = DateFilter::everything();
        assert_eq!(all.since(), "");
        assert_eq!(all.until(), "");
    }

    #[test]
    fn a_period_covers_both_of_its_days() {
        let today = chrono::Local::now().date_naive();
        let filter = DateFilter {
            preset: Preset::Period,
            from: today - chrono::Days::new(3),
            to: today - chrono::Days::new(1),
        };
        assert!(filter.period_is_ordered());
        // It starts three days back and ends when today starts: yesterday's day is
        // wholly inside it.
        assert_eq!(filter.until(), DateFilter::preset(Preset::Today).since());
        assert!(filter.since() < filter.until());
    }

    #[test]
    fn one_day_period_is_that_day() {
        let today = chrono::Local::now().date_naive();
        let filter = DateFilter {
            preset: Preset::Period,
            from: today,
            to: today,
        };
        // It starts where "today" starts, and it ends tomorrow at midnight: the
        // difference is only that "today" has no upper bound at all.
        assert_eq!(filter.since(), DateFilter::preset(Preset::Today).since());
        assert!(filter.until() > filter.since());
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        assert!(now >= filter.since() && now < filter.until(), "now is inside the day");
    }

    #[test]
    fn the_clause_compares_times_not_text() {
        let clause = DateFilter::clause("added_at", "at");
        assert!(clause.contains("?1") && clause.contains("?2"));
        assert!(clause.contains("datetime("), "offsets must not decide");
        let anonymous = DateFilter::clause_anonymous("h.at", "h.at");
        assert!(anonymous.contains("datetime("));
        assert!(!anonymous.contains("?1"), "positional statement");
    }
}
