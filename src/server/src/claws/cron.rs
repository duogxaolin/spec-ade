//! Cron parsing and next-fire calculation for Claw schedules.
//!
//! Spec: `docs/specs/SPEC-007-claws.md` §2.1, §3.3, §4 [INVENTED-2].
//!
//! **Why `croner` directly and not `tokio-cron-scheduler`** — the whole reason this
//! module exists. That crate hard-codes its parser in both constructors
//! (`job/mod.rs:129-139`, `:221-231`):
//!
//! ```text
//! CronParser::builder().seconds(Seconds::Required).dom_and_dow(true)
//! ```
//!
//! Two consequences, both verified against croner 3.0.1's source and by running it:
//!
//! 1. `Seconds::Required` **rejects** a 5-field pattern outright — `parser.rs:120-122`
//!    captures the field count *before* normalisation, then `:140-149` errors unless it
//!    is 6 or 7. The product docs promise "standard 5-field cron (`0 9 * * *`)"
//!    (`claws.mdx:66`), so every expression a user already knows would be an error.
//! 2. `dom_and_dow(true)` **ANDs** day-of-month with day-of-week (`pattern.rs:126-139`),
//!    while crontab ORs them. `0 9 13 * FRI` would fire only on Friday the 13th instead
//!    of on the 13th *or* any Friday — a wrong schedule with no error to notice.
//!
//! [`Seconds::Optional`] accepts **both** widths, and `dom_and_dow(false)` restores
//! crontab's OR. That is the whole of what this phase needed from the scheduler crate,
//! so the dependency is gone and the parser config lives here where it is testable.
//!
//! Everything here is pure: a `&str` in, a parse result or a `DateTime<Utc>` out. No
//! filesystem, no HTTP, no `Utc::now()` except through an explicit argument.

use chrono::{DateTime, Utc};
use croner::Cron;
use croner::parser::{CronParser, Seconds};

/// A validated Claw schedule expression.
///
/// Holds the compiled pattern plus the string the user typed: the UI echoes the
/// original back, and re-rendering it from the pattern would show a normalised form
/// the user never wrote.
#[derive(Debug, Clone)]
pub struct Schedule {
    pattern: Cron,
    source: String,
}

/// Why a cron expression was refused.
///
/// Carries croner's own message rather than a generic "invalid": the difference
/// between "wrong number of fields" and "hour 25 out of bounds" is exactly what the
/// user needs to fix it.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid cron expression: {0}")]
pub struct CronError(pub String);

impl Schedule {
    /// Parse `expr`, accepting 5, 6, or 7 fields plus croner's `@`-macros.
    ///
    /// Rejected on purpose (croner does not implement them): `@reboot`, `@midnight`,
    /// `@minutely`. Accepting them by rewriting would mean inventing semantics —
    /// `@reboot` in particular has no meaning for a schedule that is re-evaluated
    /// from the current time on every server start.
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let source = expr.trim().to_string();
        if source.is_empty() {
            return Err(CronError("expression is empty".to_string()));
        }
        let pattern = parser()
            .parse(&source)
            .map_err(|e| CronError(e.to_string()))?;
        Ok(Self { pattern, source })
    }

    /// The expression as the user typed it (trimmed).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Human-readable rendering, e.g. `"At 09:00."`.
    ///
    /// Echoed by `POST`/`PUT` so the user can see the server agreed with them
    /// (SPEC-007 §3.3, deliverable #4) — the cheapest possible guard against a
    /// silently misread schedule.
    pub fn describe(&self) -> String {
        self.pattern.describe()
    }

    /// First fire strictly after `after`, or `None` if the pattern can never match
    /// again (a pinned year in the past).
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.pattern.find_next_occurrence(&after, false).ok()
    }
}

/// The parser config, in one place so a test can prove the two choices hold.
fn parser() -> CronParser {
    CronParser::builder()
        .seconds(Seconds::Optional)
        .dom_and_dow(false)
        .build()
}

/// Earliest next fire across `schedules`, ignoring the ones that never fire again.
pub fn earliest_next<'a>(
    schedules: impl IntoIterator<Item = &'a Schedule>,
    after: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    schedules
        .into_iter()
        .filter_map(|s| s.next_after(after))
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A fixed instant so every assertion below is about the pattern, not the clock.
    /// Sunday 2026-11-01 00:00:00 UTC — chosen because November 2026 has a Friday the
    /// 13th, which is what makes the DOM/DOW test (E5) able to fail.
    fn base() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap()
    }

    fn next(expr: &str) -> String {
        Schedule::parse(expr)
            .unwrap()
            .next_after(base())
            .unwrap()
            .format("%Y-%m-%d %a %H:%M:%S")
            .to_string()
    }

    #[test]
    fn accepts_five_field_crontab() {
        // E1. The syntax the product docs promise (`claws.mdx:66`) and the one
        // `tokio-cron-scheduler` would have rejected outright.
        assert_eq!(next("0 9 * * *"), "2026-11-01 Sun 09:00:00");
        assert_eq!(next("*/5 * * * *"), "2026-11-01 Sun 00:05:00");
        assert_eq!(next("0 9 * * MON-FRI"), "2026-11-02 Mon 09:00:00");
    }

    #[test]
    fn accepts_six_and_seven_field_forms() {
        // E2. Six fields means the first is *seconds*, so "0 0 9 * * *" is the same
        // 09:00 as the five-field "0 9 * * *" — if the widths were confused this
        // would come out at 00:09.
        assert_eq!(next("0 0 9 * * *"), "2026-11-01 Sun 09:00:00");
        assert_eq!(next("30 0 9 * * *"), "2026-11-01 Sun 09:00:30");
        // Seven fields pins the year.
        assert_eq!(next("0 0 9 * * * 2030"), "2030-01-01 Tue 09:00:00");
    }

    #[test]
    fn day_of_month_and_day_of_week_are_ored() {
        // E5 — the assertion that catches `dom_and_dow(true)`.
        //
        // crontab semantics: "day 13 OR Friday". November 2026 starts on a Sunday,
        // so the first Friday is the 6th and the 13th is itself a Friday. With AND
        // the answer would be the 13th; with OR it is the 6th.
        assert_eq!(next("0 9 13 * FRI"), "2026-11-06 Fri 09:00:00");
        // Both halves alone still work, which rules out "the DOW field is ignored"
        // as an alternative explanation for the line above.
        assert_eq!(next("0 9 13 * *"), "2026-11-13 Fri 09:00:00");
        assert_eq!(next("0 9 * * FRI"), "2026-11-06 Fri 09:00:00");
    }

    #[test]
    fn rejects_wrong_field_counts() {
        // E3.
        let err = Schedule::parse("0 9 * *").unwrap_err();
        assert!(err.0.contains("fields"), "unhelpful message: {}", err.0);
        assert!(Schedule::parse("0 9").is_err());
        assert!(Schedule::parse("0 0 0 9 * * * *").is_err());
    }

    #[test]
    fn rejects_out_of_range_components() {
        // E4. Field count is right, the value is not — a distinct croner error, and
        // the user needs to be told which kind it was.
        assert!(Schedule::parse("0 25 * * *").is_err());
        assert!(Schedule::parse("99 9 * * *").is_err());
        assert!(Schedule::parse("0 9 32 * *").is_err());
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        // E7.
        assert_eq!(
            Schedule::parse("   ").unwrap_err(),
            CronError("expression is empty".to_string())
        );
        assert!(Schedule::parse("").is_err());
    }

    #[test]
    fn supports_the_macros_croner_implements_and_no_others() {
        // E6. Supporting a macro we cannot evaluate would mean inventing a schedule.
        for ok in [
            "@daily",
            "@hourly",
            "@weekly",
            "@monthly",
            "@yearly",
            "@annually",
        ] {
            assert!(Schedule::parse(ok).is_ok(), "{ok} should parse");
        }
        for bad in ["@reboot", "@midnight", "@minutely", "@every_minute"] {
            assert!(Schedule::parse(bad).is_err(), "{bad} should be refused");
        }
        assert_eq!(next("@daily"), "2026-11-02 Mon 00:00:00");
    }

    #[test]
    fn describe_is_non_empty_and_source_is_verbatim() {
        // E8.
        let s = Schedule::parse("  0 9 * * *  ").unwrap();
        assert_eq!(
            s.source(),
            "0 9 * * *",
            "source must be trimmed, not reflowed"
        );
        assert!(s.describe().contains("09:00"), "describe: {}", s.describe());
    }

    #[test]
    fn earliest_next_picks_the_soonest() {
        let morning = Schedule::parse("0 9 * * *").unwrap();
        let five_min = Schedule::parse("*/5 * * * *").unwrap();
        let got = earliest_next([&morning, &five_min], base()).unwrap();
        assert_eq!(got, five_min.next_after(base()).unwrap());
        assert_eq!(earliest_next(std::iter::empty(), base()), None);
    }

    #[test]
    fn a_year_in_the_past_never_fires_again() {
        // `next_after` returning `None` is what stops the runtime from parking on a
        // schedule that can never come — an unwrap here would panic the task.
        let past = Schedule::parse("0 0 9 * * * 2020").unwrap();
        assert_eq!(past.next_after(base()), None);
    }
}
