//! Which holiday microphone is shown on a given day.
//!
//! The date comes from the local clock; nothing here touches the network. Fixed
//! holidays are matched by month and day, the lunar ones come from small tables,
//! Easter is computed, and Friday the 13th is derived rather than stored.
//!
//! Two details are deliberate:
//! - the end-of-year tree is shown around the holiday, so the tray is decorated
//!   in the last week of December and the first week of January, with the clock
//!   on the night itself;
//! - 23 February, 8 March and Maslenitsa are Russian-only, because they are the
//!   only holidays here that belong to one language rather than one date.
use chrono::{Datelike, Duration, NaiveDate, Weekday};

/// Order in which competing holidays are resolved: the smallest number wins.
/// Recording and file processing are handled by the tray and always outrank any
/// holiday.
fn priority(id: &str) -> u8 {
    match id {
        "newyear" => 1,
        "cny" => 2,
        "love" => 3,
        "friday" => 4,
        "clover" => 5,
        "easter" => 6,
        "space" => 7,
        "victory" => 8,
        "halloween" => 9,
        "diwali" => 10,
        "dead" => 11,
        "christmas" => 12,
        "hanukkah" => 13,
        "eid" => 14,
        "defender" => 15,
        "women" => 16,
        "maslenitsa" => 17,
        _ => u8::MAX,
    }
}

/// Lunar holidays, first day only. The Chinese New Year and Eid tables cover the
/// years the app can expect to run through; a year outside them simply shows no
/// holiday instead of a wrong one.
const CHINESE_NEW_YEAR: [(i32, u32, u32); 11] = [
    (2025, 1, 29),
    (2026, 2, 17),
    (2027, 2, 6),
    (2028, 1, 26),
    (2029, 2, 13),
    (2030, 2, 3),
    (2031, 1, 23),
    (2032, 2, 11),
    (2033, 1, 31),
    (2034, 2, 19),
    (2035, 2, 8),
];

const DIWALI: [(i32, u32, u32); 6] = [
    (2025, 10, 20),
    (2026, 11, 8),
    (2027, 10, 29),
    (2028, 10, 17),
    (2029, 11, 5),
    (2030, 10, 26),
];

const EID_AL_FITR: [(i32, u32, u32); 6] = [
    (2025, 3, 30),
    (2026, 3, 20),
    (2027, 3, 9),
    (2028, 2, 26),
    (2029, 2, 14),
    (2030, 2, 4),
];

/// First candle of Hanukkah, 25 Kislev.
const HANUKKAH: [(i32, u32, u32); 6] = [
    (2025, 12, 14),
    (2026, 12, 4),
    (2027, 12, 24),
    (2028, 12, 12),
    (2029, 12, 1),
    (2030, 12, 20),
];

fn from_table(table: &[(i32, u32, u32)], date: NaiveDate) -> Option<NaiveDate> {
    table
        .iter()
        .find(|(year, _, _)| *year == date.year())
        .and_then(|(year, month, day)| NaiveDate::from_ymd_opt(*year, *month, *day))
}

/// Western (Gregorian) Easter, Meeus/Jones/Butcher.
pub fn western_easter(year: i32) -> Option<NaiveDate> {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    NaiveDate::from_ymd_opt(year, month as u32, day as u32)
}

/// Orthodox Easter: the Julian computus, moved to the Gregorian calendar by the
/// 13-day offset that holds from 1900 to 2099.
pub fn orthodox_easter(year: i32) -> Option<NaiveDate> {
    let a = year % 4;
    let b = year % 7;
    let c = year % 19;
    let d = (19 * c + 15) % 30;
    let e = (2 * a + 4 * b - d + 34) % 7;
    let month = (d + e + 114) / 31;
    let day = (d + e + 114) % 31 + 1;
    NaiveDate::from_ymd_opt(year, month as u32, day as u32)?.checked_add_signed(Duration::days(13))
}

/// Maslenitsa is the week before Great Lent, which starts 48 days before
/// Orthodox Easter: the week runs from Easter − 55 to Easter − 49.
fn maslenitsa_range(year: i32) -> Option<(NaiveDate, NaiveDate)> {
    let easter = orthodox_easter(year)?;
    Some((
        easter.checked_sub_signed(Duration::days(55))?,
        easter.checked_sub_signed(Duration::days(49))?,
    ))
}

fn is_russian(language: &str) -> bool {
    language.eq_ignore_ascii_case("ru")
}

/// Every holiday that has a claim on `date`, before the priority rule.
fn candidates(date: NaiveDate, language: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    let (month, day) = (date.month(), date.day());
    let year = date.year();

    // The decorated season: the last week of December and the first of January.
    if month == 12 && (24..=30).contains(&day) {
        found.push("christmas");
    }
    if (month == 12 && day == 31) || (month == 1 && day == 1) {
        found.push("newyear");
    }
    if month == 1 && (2..=8).contains(&day) {
        found.push("christmas");
    }

    match (month, day) {
        (2, 14) => found.push("love"),
        (2, 23) if is_russian(language) => found.push("defender"),
        (3, 8) if is_russian(language) => found.push("women"),
        (3, 17) => found.push("clover"),
        (4, 12) => found.push("space"),
        (5, 9) => found.push("victory"),
        (10, 31) => found.push("halloween"),
        (11, 2) => found.push("dead"),
        (12, 25) | (1, 7) => found.push("christmas"),
        _ => {}
    }

    if let Some(start) = from_table(&CHINESE_NEW_YEAR, date) {
        // The celebration runs for the first three days.
        if date >= start && date <= start + Duration::days(2) {
            found.push("cny");
        }
    }
    if let Some(start) = from_table(&DIWALI, date) {
        if date == start {
            found.push("diwali");
        }
    }
    if let Some(start) = from_table(&EID_AL_FITR, date) {
        if date == start {
            found.push("eid");
        }
    }
    if let Some(start) = from_table(&HANUKKAH, date) {
        if date >= start && date <= start + Duration::days(7) {
            found.push("hanukkah");
        }
    }
    if let Some(easter) = western_easter(year) {
        if date == easter {
            found.push("easter");
        }
    }
    if let Some(easter) = orthodox_easter(year) {
        if date == easter {
            found.push("easter");
        }
    }
    if is_russian(language) {
        if let Some((start, end)) = maslenitsa_range(year) {
            if date >= start && date <= end {
                found.push("maslenitsa");
            }
        }
    }
    if date.weekday() == Weekday::Fri && day == 13 {
        found.push("friday");
    }

    found
}

/// The holiday the process is showing right now, or `None` on an ordinary day.
///
/// The daemon computes it once per change and publishes it here, so the tray,
/// the phone API and the web page cannot disagree about what today is.
pub fn current() -> Option<String> {
    current_lock().read().unwrap().clone()
}

/// Publish the holiday the tray is drawing. Called by the daemon when the date,
/// the language or the setting changes.
pub fn set_current(id: Option<String>) {
    *current_lock().write().unwrap() = id;
}

fn current_lock() -> &'static std::sync::RwLock<Option<String>> {
    static CURRENT: std::sync::OnceLock<std::sync::RwLock<Option<String>>> = std::sync::OnceLock::new();
    CURRENT.get_or_init(|| std::sync::RwLock::new(None))
}

/// The holiday to show on `date`, or `None` for an ordinary day.
///
/// `language` is the interface language: `ru` enables the three Russian-only
/// holidays.
pub fn active(date: NaiveDate, language: &str) -> Option<&'static str> {
    candidates(date, language)
        .into_iter()
        .min_by_key(|id| priority(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
    }

    #[test]
    fn the_clock_wins_on_new_years_night() {
        assert_eq!(active(day(2027, 1, 1), "en"), Some("newyear"));
        assert_eq!(active(day(2026, 12, 31), "en"), Some("newyear"));
    }

    #[test]
    fn the_tree_is_up_around_the_holiday() {
        for day_of_month in [24, 25, 26, 30] {
            assert_eq!(active(day(2026, 12, day_of_month), "en"), Some("christmas"), "{day_of_month}");
        }
        for day_of_month in [2, 5, 7, 8] {
            assert_eq!(active(day(2027, 1, day_of_month), "en"), Some("christmas"), "{day_of_month}");
        }
        // The season is over on 9 January and has not started on 23 December.
        assert_eq!(active(day(2027, 1, 9), "en"), None);
        assert_eq!(active(day(2026, 12, 23), "en"), None);
    }

    #[test]
    fn the_chinese_new_year_lasts_three_days() {
        assert_eq!(active(day(2026, 2, 17), "en"), Some("cny"));
        assert_eq!(active(day(2026, 2, 18), "en"), Some("cny"));
        assert_eq!(active(day(2026, 2, 19), "en"), Some("cny"));
        assert_eq!(active(day(2026, 2, 20), "en"), None);
    }

    #[test]
    fn the_russian_only_holidays_depend_on_the_language() {
        assert_eq!(active(day(2026, 2, 23), "ru"), Some("defender"));
        assert_eq!(active(day(2026, 2, 23), "en"), None);
        assert_eq!(active(day(2026, 3, 8), "ru"), Some("women"));
        assert_eq!(active(day(2026, 3, 8), "zh"), None);
        // Maslenitsa 2026 is 16–22 February.
        assert_eq!(active(day(2026, 2, 16), "ru"), Some("maslenitsa"));
        assert_eq!(active(day(2026, 2, 22), "ru"), Some("maslenitsa"));
        // In 2026 the Chinese New Year (17–19 February) falls inside that week
        // and outranks it, so the days it covers show the red envelope.
        assert_eq!(active(day(2026, 2, 18), "ru"), Some("cny"));
        assert_eq!(active(day(2026, 2, 16), "en"), None);
    }

    #[test]
    fn easter_is_computed_for_both_calendars() {
        assert_eq!(western_easter(2026), Some(day(2026, 4, 5)));
        assert_eq!(orthodox_easter(2026), Some(day(2026, 4, 12)));
        assert_eq!(western_easter(2025), Some(day(2025, 4, 20)));
        assert_eq!(orthodox_easter(2025), Some(day(2025, 4, 20)));
        assert_eq!(western_easter(2027), Some(day(2027, 3, 28)));
        assert_eq!(orthodox_easter(2027), Some(day(2027, 5, 2)));
    }

    #[test]
    fn easter_outranks_cosmonautics_day_when_they_share_a_date() {
        // 12 April 2026 is both Orthodox Easter and Cosmonautics Day.
        assert_eq!(active(day(2026, 4, 12), "ru"), Some("easter"));
        assert_eq!(active(day(2027, 4, 12), "en"), Some("space"));
    }

    #[test]
    fn friday_the_thirteenth_is_derived() {
        // 13 November 2026 is a Friday.
        assert_eq!(active(day(2026, 11, 13), "en"), Some("friday"));
        // 13 October 2026 is a Tuesday, 13 February 2026 a Friday.
        assert_eq!(active(day(2026, 10, 13), "en"), None);
        assert_eq!(active(day(2026, 2, 13), "en"), Some("friday"));
        // 14 February outranks the following Friday only by its own date.
        assert_eq!(active(day(2026, 2, 14), "en"), Some("love"));
    }

    #[test]
    fn the_lunar_tables_show_only_known_years() {
        assert_eq!(active(day(2026, 11, 8), "en"), Some("diwali"));
        assert_eq!(active(day(2026, 3, 20), "en"), Some("eid"));
        assert_eq!(active(day(2026, 12, 4), "en"), Some("hanukkah"));
        assert_eq!(active(day(2026, 12, 11), "en"), Some("hanukkah"));
        // Beyond the tables those holidays simply do not appear.
        assert_eq!(active(day(2040, 11, 8), "en"), None);
    }

    #[test]
    fn an_ordinary_day_has_no_holiday() {
        assert_eq!(active(day(2026, 6, 15), "en"), None);
        assert_eq!(active(day(2026, 6, 15), "ru"), None);
    }
}
