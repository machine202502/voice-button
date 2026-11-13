// Vendored from `egui_extras` 0.27.2 (`src/datepicker`, MIT OR Apache-2.0),
// with every user-visible string routed through `tr()`: the crate hardcodes
// English month names, weekday heads and the Save/Cancel buttons, which put
// English text into a Russian window. The widget logic is unchanged.

mod button;
mod popup;

pub use button::{open_picker, DatePickerButton};
use chrono::{Datelike, Duration, NaiveDate, Weekday};

#[derive(Debug)]
struct Week {
    number: u8,
    days: Vec<NaiveDate>,
}

/// The person's own day. The crate asked UTC, which in UTC+5 is already
/// tomorrow every evening — the highlighted "today" was the wrong cell.
pub fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

fn month_data(year: i32, month: u32) -> Vec<Week> {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("Could not create NaiveDate");
    let mut start = first;
    while start.weekday() != Weekday::Mon {
        start = start.checked_sub_signed(Duration::days(1)).unwrap();
    }
    let mut weeks = vec![];
    let mut week = vec![];
    while start < first || start.month() == first.month() || start.weekday() != Weekday::Mon {
        week.push(start);

        if start.weekday() == Weekday::Sun {
            weeks.push(Week {
                number: start.iso_week().week() as u8,
                days: std::mem::take(&mut week),
            });
        }
        start = start.checked_add_signed(Duration::days(1)).unwrap();
    }

    weeks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_starts_on_monday_and_covers_the_whole_month() {
        let weeks = month_data(2026, 11);
        assert!(!weeks.is_empty());
        for week in &weeks {
            assert_eq!(week.days.len(), 7, "a week has seven cells");
            assert_eq!(week.days[0].weekday(), Weekday::Mon, "weeks start on Monday");
        }
        let first = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let last = NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
        let cells: Vec<NaiveDate> = weeks.iter().flat_map(|week| week.days.clone()).collect();
        assert!(cells.contains(&first) && cells.contains(&last));
    }

    #[test]
    fn today_is_the_local_day_not_utc() {
        // The bug this pins: in a positive offset the UTC date is behind, so the
        // calendar highlighted yesterday for the first hours of the evening.
        let local = chrono::Local::now().date_naive();
        assert_eq!(today(), local);
    }

    #[test]
    fn two_clicks_pick_a_period_in_one_calendar() {
        // The person clicks two days in the same grid; the second click closes
        // the span, whichever end they clicked first.
        let mut state = popup::DatePickerPopupState::default();
        let first = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let second = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        assert_eq!(popup::pick_day(&mut state, first), None, "one click is not a period yet");
        assert_eq!(popup::pick_day(&mut state, second), Some((first, second)));
        assert_eq!(state.pending, None, "the span is finished");

        let mut state = popup::DatePickerPopupState::default();
        assert_eq!(popup::pick_day(&mut state, second), None, "the later day can be the start");
        assert_eq!(
            popup::pick_day(&mut state, first),
            Some((first, second)),
            "the period is the same whichever end comes first"
        );
    }

    #[test]
    fn a_third_click_starts_another_span() {
        // The span is not applied until Save, so the calendar has to let the
        // person pick a different one straight away.
        let mut state = popup::DatePickerPopupState::default();
        let first = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let second = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let again = NaiveDate::from_ymd_opt(2026, 12, 3).unwrap();
        popup::pick_day(&mut state, first);
        popup::pick_day(&mut state, second);
        assert_eq!(state.span_dates(), Some((first, second)));
        assert_eq!(popup::pick_day(&mut state, again), None, "a new span begins");
        assert_eq!(state.span_dates(), Some((again, again)));
    }
}
