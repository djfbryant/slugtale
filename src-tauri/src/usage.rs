//! Usage (CONTEXT.md, ADR-0025): opt-in Daily Usage Records and the Time Saved
//! computed from them.
//!
//! Everything here is aggregate. A Daily Usage Record holds a local date, a
//! dictation count, a word count, and a speaking duration — no transcription,
//! no audio, no text target, no app name. That is what keeps this outside
//! Dictation History (ADR-0002).
//!
//! Time Saved is never stored. It is derived from the stored words and duration
//! and whatever the Typing Baseline says right now, so redoing the Typing
//! Challenges moves historical Time Saved rather than leaving a frozen number
//! that no longer matches the user's typing.

use serde::{Deserialize, Serialize};

/// A local calendar date, stored as the ISO `YYYY-MM-DD` string a Daily Usage
/// Record is keyed by.
///
/// This is a plain date with no time and no zone on purpose: a Counted Segment
/// belongs to the local date it landed on, and moving to another timezone must
/// not rewrite the days already recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl LocalDate {
    pub fn new(year: i32, month: u32, day: u32) -> Self {
        Self { year, month, day }
    }

    /// Parse the `YYYY-MM-DD` form a Usage File stores. Returns `None` for
    /// anything else, so a hand-edited or corrupt record is dropped rather than
    /// counted into the wrong day.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let year = text[0..4].parse::<i32>().ok()?;
        let month = text[5..7].parse::<u32>().ok()?;
        let day = text[8..10].parse::<u32>().ok()?;
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        Some(Self { year, month, day })
    }

    pub fn to_iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Days since 1970-01-01, used only to compare and step between dates.
    /// Proleptic Gregorian, which is what every date this app will ever see is.
    pub fn days_from_epoch(self) -> i64 {
        // Shift the year so leap days land at the end of the cycle, which makes
        // the century rules a plain division instead of a special case.
        let year = if self.month <= 2 {
            self.year - 1
        } else {
            self.year
        } as i64;
        let era = if year >= 0 { year } else { year - 399 } / 400;
        let year_of_era = year - era * 400;
        let month = self.month as i64;
        let day = self.day as i64;
        let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }

    /// Which day of the week this date falls on.
    pub fn weekday(self) -> Weekday {
        // 1970-01-01 was a Thursday.
        let index = (self.days_from_epoch() + 4).rem_euclid(7);
        match index {
            0 => Weekday::Sunday,
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            _ => Weekday::Saturday,
        }
    }

    /// The first date of the calendar week containing this date, given where the
    /// user's locale starts its weeks.
    pub fn week_start(self, week_start: WeekStart) -> Self {
        let back = (self.weekday().index() + 7 - week_start.weekday().index()) % 7;
        self.minus_days(back as i64)
    }

    fn minus_days(self, days: i64) -> Self {
        Self::from_days_from_epoch(self.days_from_epoch() - days)
    }

    fn from_days_from_epoch(days: i64) -> Self {
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_index = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_index + 2) / 5 + 1;
        let month = month_index + if month_index < 10 { 3 } else { -9 };
        Self {
            year: (year + i64::from(month <= 2)) as i32,
            month: month as u32,
            day: day as u32,
        }
    }
}

/// Today's date in the machine's local timezone.
///
/// Local, not UTC, and resolved at the moment a Counted Segment lands rather
/// than cached: a dictation at 00:05 belongs to the new day, and a user who
/// flies somewhere else gets the local date there without their earlier days
/// being rewritten.
pub fn today_local() -> LocalDate {
    use chrono::Datelike;
    let now = chrono::Local::now().date_naive();
    LocalDate::new(now.year(), now.month(), now.day())
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl Weekday {
    fn index(self) -> usize {
        match self {
            Self::Sunday => 0,
            Self::Monday => 1,
            Self::Tuesday => 2,
            Self::Wednesday => 3,
            Self::Thursday => 4,
            Self::Friday => 5,
            Self::Saturday => 6,
        }
    }
}

/// Which day the user's locale calls the first of the week. Resolved from the OS
/// at the platform boundary rather than assumed here, because "this week" means
/// a different set of days to a user in London and a user in Chicago.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekStart {
    Sunday,
    Monday,
}

impl Default for WeekStart {
    /// ISO-8601, which is the safe answer when the OS will not say.
    fn default() -> Self {
        Self::Monday
    }
}

impl WeekStart {
    fn weekday(self) -> Weekday {
        match self {
            Self::Sunday => Weekday::Sunday,
            Self::Monday => Weekday::Monday,
        }
    }
}

/// One local day's counted dictation totals (CONTEXT.md). This is the whole of
/// what Usage stores per day, and it is deliberately impossible to reconstruct
/// anything the user said from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyUsageRecord {
    /// The local date, `YYYY-MM-DD`.
    pub date: String,
    /// Dictations — start-to-stop, not segments — that put text into a target.
    pub dictations: u32,
    /// Words across every Counted Segment of those dictations.
    pub words: u32,
    /// Captured audio duration, in seconds, for those Counted Segments.
    pub speaking_seconds: f64,
}

impl DailyUsageRecord {
    fn empty(date: LocalDate) -> Self {
        Self {
            date: date.to_iso(),
            dictations: 0,
            words: 0,
            speaking_seconds: 0.0,
        }
    }
}

/// The Usage File (CONTEXT.md): a sibling of the Settings File that exists only
/// while the user has chosen to store Usage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageFile {
    #[serde(default)]
    pub days: Vec<DailyUsageRecord>,
}

/// Totals over some span of Daily Usage Records. Time Saved is not in here
/// because it is not a stored quantity; ask [`time_saved_seconds`] for it with
/// the current Typing Baseline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub dictations: u32,
    pub words: u32,
    pub speaking_seconds: f64,
}

impl UsageTotals {
    fn add(&mut self, record: &DailyUsageRecord) {
        self.dictations = self.dictations.saturating_add(record.dictations);
        self.words = self.words.saturating_add(record.words);
        self.speaking_seconds += record.speaking_seconds;
    }
}

/// One Counted Segment's contribution to Usage: a segment that was inserted or
/// rescued, and therefore actually reached the user's document.
///
/// `starts_dictation` is what makes the dictation count a count of dictations
/// rather than of segments. The first Counted Segment of a start-to-stop sets
/// it; every Pause Flush after that adds words and duration to the same day
/// without adding a second dictation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CountedSegment {
    pub words: u32,
    pub speaking_seconds: f64,
    pub starts_dictation: bool,
}

/// Count the words of a cleaned transcription the way Usage means them: split on
/// whitespace, same as the Typing Challenge scores typed words, so Time Saved
/// compares like with like.
pub fn count_words(text: &str) -> u32 {
    text.split_whitespace().count() as u32
}

/// Add one Counted Segment to the day it landed on.
///
/// A Counted Segment belongs to the local date of its insert or rescue, so a
/// dictation started before midnight and finished after it puts its later
/// segments on the later day. That is the honest answer and it means old days
/// are never rewritten.
pub fn record_counted_segment(usage: &mut UsageFile, date: LocalDate, segment: CountedSegment) {
    let iso = date.to_iso();
    let record = match usage.days.iter_mut().find(|record| record.date == iso) {
        Some(record) => record,
        None => {
            usage.days.push(DailyUsageRecord::empty(date));
            usage.days.sort_by(|a, b| a.date.cmp(&b.date));
            usage
                .days
                .iter_mut()
                .find(|record| record.date == iso)
                .expect("the record was just inserted")
        }
    };

    if segment.starts_dictation {
        record.dictations = record.dictations.saturating_add(1);
    }
    record.words = record.words.saturating_add(segment.words);
    record.speaking_seconds += segment.speaking_seconds.max(0.0);
}

/// Totals for exactly one local day.
pub fn totals_for_day(usage: &UsageFile, date: LocalDate) -> UsageTotals {
    let iso = date.to_iso();
    let mut totals = UsageTotals::default();
    for record in usage.days.iter().filter(|record| record.date == iso) {
        totals.add(record);
    }
    totals
}

/// Totals for the calendar week containing `date`, from the locale's first day
/// of the week up to and including `date`.
pub fn totals_for_week(usage: &UsageFile, date: LocalDate, week_start: WeekStart) -> UsageTotals {
    let first = date.week_start(week_start).days_from_epoch();
    let last = date.days_from_epoch();
    let mut totals = UsageTotals::default();
    for record in &usage.days {
        let Some(day) = LocalDate::parse(&record.date) else {
            continue;
        };
        let day = day.days_from_epoch();
        if day >= first && day <= last {
            totals.add(record);
        }
    }
    totals
}

/// Totals across every Daily Usage Record in the file.
pub fn totals_all_time(usage: &UsageFile) -> UsageTotals {
    let mut totals = UsageTotals::default();
    for record in &usage.days {
        totals.add(record);
    }
    totals
}

/// Time Saved in seconds: how long these words would have taken to type at the
/// user's Typing Baseline, minus how long they actually spent speaking them.
///
/// Floored at zero, because "you lost time" is not a number this pane is for —
/// and a short dictation of one word can easily take longer to say than to type.
/// Returns `None` when there is no baseline at all, which is the hole the pane
/// shows with a take-the-baseline action rather than an invented default WPM.
pub fn time_saved_seconds(totals: &UsageTotals, words_per_minute: Option<u32>) -> Option<f64> {
    let wpm = words_per_minute?;
    if wpm == 0 {
        return None;
    }
    let typing_seconds = f64::from(totals.words) / f64::from(wpm) * 60.0;
    Some((typing_seconds - totals.speaking_seconds).max(0.0))
}

/// Render Time Saved the way the Usage pane states it: prefixed with "About",
/// no decimals, and never a false precision the estimate cannot support.
///
/// `None` is the hole — the user has no Typing Baseline yet, so there is no
/// honest number to show.
pub fn format_time_saved(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds else {
        return "—".to_string();
    };
    if seconds <= 0.0 {
        return "0 min".to_string();
    }
    if seconds < 60.0 {
        return "Less than a minute".to_string();
    }

    let minutes = (seconds / 60.0).round() as i64;
    if minutes < 60 {
        return format!("About {minutes} min");
    }

    let hours = minutes / 60;
    let rest = minutes % 60;
    if rest == 0 {
        format!("About {hours} hr")
    } else {
        format!("About {hours} hr {rest} min")
    }
}

/// Write the Usage File as human-readable JSON, replacing it atomically so a
/// crash mid-write cannot leave a half-parsed file where counts used to be.
/// Mirrors [`crate::save_settings`] deliberately: same discipline, separate file.
pub fn save_usage(path: &std::path::Path, usage: &UsageFile) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(usage)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("usage.json");
    let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));

    std::fs::write(&temp_path, json)?;
    match std::fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

/// Load the Usage File, treating missing and unreadable alike as "no days yet".
///
/// A missing file is the normal state: it does not exist until the user turns
/// storing on, and turning it off deletes it.
pub fn load_usage(path: &std::path::Path) -> UsageFile {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Delete the Usage File. Turning the store toggle off must leave nothing
/// behind, so an already-absent file is success rather than an error.
pub fn delete_usage(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> LocalDate {
        LocalDate::new(year, month, day)
    }

    fn segment(words: u32, seconds: f64, starts: bool) -> CountedSegment {
        CountedSegment {
            words,
            speaking_seconds: seconds,
            starts_dictation: starts,
        }
    }

    #[test]
    fn a_dictation_with_several_pause_flushes_counts_once() {
        // This is the whole reason a Counted Segment carries `starts_dictation`:
        // one start-to-stop that flushed three times is one dictation, not three.
        let mut usage = UsageFile::default();
        let today = date(2026, 8, 17);

        record_counted_segment(&mut usage, today, segment(10, 4.0, true));
        record_counted_segment(&mut usage, today, segment(8, 3.5, false));
        record_counted_segment(&mut usage, today, segment(6, 2.5, false));

        let totals = totals_for_day(&usage, today);
        assert_eq!(totals.dictations, 1);
        assert_eq!(totals.words, 24);
        assert_eq!(totals.speaking_seconds, 10.0);
    }

    #[test]
    fn a_dictation_that_crosses_midnight_leaves_the_earlier_day_alone() {
        // Segments belong to the date they landed on, so the late-night day keeps
        // what it already recorded and the new day starts its own record.
        let mut usage = UsageFile::default();
        record_counted_segment(&mut usage, date(2026, 8, 17), segment(12, 5.0, true));
        record_counted_segment(&mut usage, date(2026, 8, 18), segment(9, 4.0, false));

        assert_eq!(
            totals_for_day(&usage, date(2026, 8, 17)),
            UsageTotals {
                dictations: 1,
                words: 12,
                speaking_seconds: 5.0
            }
        );
        assert_eq!(
            totals_for_day(&usage, date(2026, 8, 18)),
            UsageTotals {
                dictations: 0,
                words: 9,
                speaking_seconds: 4.0
            }
        );
    }

    #[test]
    fn daily_records_stay_sorted_by_date_however_they_arrive() {
        let mut usage = UsageFile::default();
        record_counted_segment(&mut usage, date(2026, 8, 18), segment(1, 1.0, true));
        record_counted_segment(&mut usage, date(2026, 8, 16), segment(1, 1.0, true));
        record_counted_segment(&mut usage, date(2026, 8, 17), segment(1, 1.0, true));

        let dates = usage
            .days
            .iter()
            .map(|record| record.date.as_str())
            .collect::<Vec<_>>();
        assert_eq!(dates, ["2026-08-16", "2026-08-17", "2026-08-18"]);
    }

    #[test]
    fn a_daily_usage_record_stores_counts_and_nothing_that_could_be_read_back() {
        // ADR-0002 holds only if the stored shape genuinely cannot carry text.
        let mut usage = UsageFile::default();
        record_counted_segment(&mut usage, date(2026, 8, 17), segment(4, 2.0, true));

        let json = serde_json::to_string(&usage).unwrap();
        assert_eq!(
            json,
            r#"{"days":[{"date":"2026-08-17","dictations":1,"words":4,"speaking_seconds":2.0}]}"#
        );
    }

    #[test]
    fn words_are_whitespace_separated_however_the_engine_spaced_them() {
        assert_eq!(count_words("Hello from slugtale"), 3);
        assert_eq!(count_words("  leading and   trailing  "), 3);
        assert_eq!(count_words(" This is the second paragraph."), 5);
        assert_eq!(count_words("   "), 0);
        assert_eq!(count_words(""), 0);
    }

    #[test]
    fn this_week_follows_the_locale_first_day_rather_than_a_fixed_monday() {
        // 2026-08-17 is a Monday. To a Monday-start locale the week begins that
        // day; to a Sunday-start locale it began the day before, so the Sunday's
        // dictation is in this week for one user and not the other.
        let mut usage = UsageFile::default();
        record_counted_segment(&mut usage, date(2026, 8, 16), segment(10, 1.0, true));
        record_counted_segment(&mut usage, date(2026, 8, 17), segment(5, 1.0, true));

        assert_eq!(
            totals_for_week(&usage, date(2026, 8, 17), WeekStart::Monday).words,
            5
        );
        assert_eq!(
            totals_for_week(&usage, date(2026, 8, 17), WeekStart::Sunday).words,
            15
        );
    }

    #[test]
    fn this_week_excludes_last_week_and_never_counts_days_ahead_of_today() {
        let mut usage = UsageFile::default();
        // The Monday before, plus a stray future day a clock change could leave.
        record_counted_segment(&mut usage, date(2026, 8, 10), segment(100, 1.0, true));
        record_counted_segment(&mut usage, date(2026, 8, 19), segment(50, 1.0, true));
        record_counted_segment(&mut usage, date(2026, 8, 18), segment(7, 1.0, true));

        let totals = totals_for_week(&usage, date(2026, 8, 18), WeekStart::Monday);
        assert_eq!(totals.words, 7);
        assert_eq!(totals.dictations, 1);
    }

    #[test]
    fn all_time_totals_span_every_recorded_day() {
        let mut usage = UsageFile::default();
        record_counted_segment(&mut usage, date(2025, 1, 1), segment(20, 6.0, true));
        record_counted_segment(&mut usage, date(2026, 8, 17), segment(30, 9.0, true));

        assert_eq!(
            totals_all_time(&usage),
            UsageTotals {
                dictations: 2,
                words: 50,
                speaking_seconds: 15.0
            }
        );
    }

    #[test]
    fn weekday_and_week_start_agree_with_the_calendar() {
        assert_eq!(date(1970, 1, 1).weekday(), Weekday::Thursday);
        assert_eq!(date(2026, 8, 17).weekday(), Weekday::Monday);
        assert_eq!(date(2026, 8, 16).weekday(), Weekday::Sunday);
        assert_eq!(date(2024, 2, 29).weekday(), Weekday::Thursday);

        assert_eq!(
            date(2026, 8, 17).week_start(WeekStart::Monday),
            date(2026, 8, 17)
        );
        assert_eq!(
            date(2026, 8, 17).week_start(WeekStart::Sunday),
            date(2026, 8, 16)
        );
        // Across a month boundary, and across a leap day.
        assert_eq!(
            date(2026, 3, 3).week_start(WeekStart::Monday),
            date(2026, 3, 2)
        );
        assert_eq!(
            date(2024, 3, 1).week_start(WeekStart::Monday),
            date(2024, 2, 26)
        );
    }

    #[test]
    fn iso_dates_round_trip_and_nonsense_dates_are_rejected() {
        for text in ["2026-08-17", "1970-01-01", "2024-02-29"] {
            assert_eq!(LocalDate::parse(text).unwrap().to_iso(), text);
        }

        for text in [
            "2026-8-17",
            "2026-13-01",
            "2026-02-30",
            "2025-02-29",
            "not a date",
            "",
        ] {
            assert_eq!(LocalDate::parse(text), None, "{text} should not parse");
        }
    }

    #[test]
    fn time_saved_is_typing_time_minus_speaking_time() {
        // 120 words at 40 WPM is three minutes of typing; saying them took one.
        let totals = UsageTotals {
            dictations: 1,
            words: 120,
            speaking_seconds: 60.0,
        };

        assert_eq!(time_saved_seconds(&totals, Some(40)), Some(120.0));
    }

    #[test]
    fn time_saved_floors_at_zero_rather_than_showing_time_lost() {
        // One word, laboriously. Typing it would have been faster, and the pane
        // is not the place to say so.
        let totals = UsageTotals {
            dictations: 1,
            words: 1,
            speaking_seconds: 30.0,
        };

        assert_eq!(time_saved_seconds(&totals, Some(60)), Some(0.0));
        assert_eq!(
            format_time_saved(time_saved_seconds(&totals, Some(60))),
            "0 min"
        );
    }

    #[test]
    fn time_saved_is_a_hole_without_a_typing_baseline() {
        let totals = UsageTotals {
            dictations: 3,
            words: 500,
            speaking_seconds: 100.0,
        };

        assert_eq!(time_saved_seconds(&totals, None), None);
        // A zero baseline would divide by zero, so it is a hole too rather than
        // an infinity rendered as a triumphant number.
        assert_eq!(time_saved_seconds(&totals, Some(0)), None);
        assert_eq!(format_time_saved(None), "—");
    }

    #[test]
    fn redoing_the_typing_challenges_moves_historical_time_saved() {
        // Nothing stored changes; only the baseline the same totals are read
        // against. A faster typist saved less time all along.
        let totals = UsageTotals {
            dictations: 10,
            words: 1200,
            speaking_seconds: 600.0,
        };

        assert_eq!(time_saved_seconds(&totals, Some(30)), Some(1800.0));
        assert_eq!(time_saved_seconds(&totals, Some(60)), Some(600.0));
    }

    #[test]
    fn time_saved_reads_as_seconds_then_minutes_then_hours_with_no_decimals() {
        assert_eq!(format_time_saved(Some(0.0)), "0 min");
        assert_eq!(format_time_saved(Some(1.0)), "Less than a minute");
        assert_eq!(format_time_saved(Some(59.4)), "Less than a minute");
        assert_eq!(format_time_saved(Some(60.0)), "About 1 min");
        assert_eq!(format_time_saved(Some(154.0)), "About 3 min");
        assert_eq!(format_time_saved(Some(3600.0)), "About 1 hr");
        assert_eq!(format_time_saved(Some(4500.0)), "About 1 hr 15 min");
        assert_eq!(format_time_saved(Some(36_000.0)), "About 10 hr");
    }

    #[test]
    fn usage_file_round_trips_through_disk_and_deletes_completely() {
        let path = std::env::temp_dir().join(format!("slugtale-usage-{}.json", std::process::id()));
        std::fs::remove_file(&path).ok();

        let mut usage = UsageFile::default();
        record_counted_segment(&mut usage, date(2026, 8, 17), segment(42, 12.5, true));

        save_usage(&path, &usage).unwrap();
        assert_eq!(load_usage(&path), usage);

        delete_usage(&path).unwrap();
        assert!(!path.exists());
        // Opting out twice must not fail, and the file stays gone.
        delete_usage(&path).unwrap();
        assert_eq!(load_usage(&path), UsageFile::default());
    }

    #[test]
    fn an_unreadable_usage_file_reads_as_no_days_rather_than_failing() {
        // Usage must never be able to break the app, and there is nothing here
        // worth recovering: the counts are a mirror, not the user's work.
        let path = std::env::temp_dir().join(format!(
            "slugtale-usage-corrupt-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, "{ not json").unwrap();

        let usage = load_usage(&path);

        std::fs::remove_file(&path).ok();
        assert_eq!(usage, UsageFile::default());
    }

    #[test]
    fn a_day_with_an_unparseable_date_is_ignored_by_week_totals() {
        let usage = UsageFile {
            days: vec![
                DailyUsageRecord {
                    date: "yesterday".to_string(),
                    dictations: 9,
                    words: 900,
                    speaking_seconds: 90.0,
                },
                DailyUsageRecord {
                    date: "2026-08-17".to_string(),
                    dictations: 1,
                    words: 10,
                    speaking_seconds: 5.0,
                },
            ],
        };

        assert_eq!(
            totals_for_week(&usage, date(2026, 8, 17), WeekStart::Monday).words,
            10
        );
    }

    #[test]
    fn iso_parse_requires_fixed_hyphen_positions() {
        for text in ["2026/08/17", "202608-17", "2026-0817", "2026-8-17"] {
            assert_eq!(LocalDate::parse(text), None, "{text} should not parse");
        }
    }

    #[test]
    fn february_respects_leap_years() {
        assert!(LocalDate::parse("2024-02-29").is_some());
        assert_eq!(LocalDate::parse("2025-02-29"), None);
        assert_eq!(
            LocalDate::parse("2025-02-28").unwrap().to_iso(),
            "2025-02-28"
        );
    }

    #[test]
    fn month_lengths_are_enforced_at_parse_time() {
        assert_eq!(LocalDate::parse("2026-04-31"), None);
        assert_eq!(LocalDate::parse("2026-06-31"), None);
        assert_eq!(LocalDate::parse("2026-11-31"), None);
    }

    #[test]
    fn days_from_epoch_is_consistent_for_known_anchors() {
        assert_eq!(date(1970, 1, 1).days_from_epoch(), 0);
        assert_eq!(date(1970, 1, 2).days_from_epoch(), 1);
        assert_eq!(date(2024, 2, 29).days_from_epoch(), 19_782);
    }

    #[test]
    fn every_weekday_is_returned_by_some_calendar_date() {
        let mut seen = Vec::new();
        for offset in 0..14 {
            let weekday = date(2026, 1, 4 + offset).weekday();
            if !seen.contains(&weekday) {
                seen.push(weekday);
            }
        }
        assert_eq!(seen.len(), 7);
    }

    #[test]
    fn stepping_one_day_forward_increments_days_from_epoch() {
        assert_eq!(
            date(2026, 4, 1).days_from_epoch() - date(2026, 3, 31).days_from_epoch(),
            1
        );
    }
}
