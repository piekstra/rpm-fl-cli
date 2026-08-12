//! Date range flags for the portal's period-scoped reads.
//!
//! The CLI speaks ISO `YYYY-MM-DD` at every boundary (SPEC v1); the portal
//! wants `MM/DD/YYYY`. All of the calendar work — parsing, "today", and that
//! conversion — comes from [`pk_cli_core::dates`], which every family CLI
//! shares. This module only owns what is portal-specific: the flag names and
//! the year-to-date default.

use pk_cli_core::{dates, CliError};

/// A start/end date range, shared by every range-scoped read.
///
/// Named `--since` / `--until` to match `pk_cli_utility::RangeArgs` and the
/// sibling CLIs; `--start` / `--end` are accepted as aliases because they read
/// more naturally for a reporting period.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct RangeArgs {
    /// Start of the period, ISO `YYYY-MM-DD` (default: January 1 of this year).
    #[arg(long, alias = "start", value_name = "YYYY-MM-DD")]
    pub since: Option<String>,

    /// End of the period, ISO `YYYY-MM-DD` (default: today).
    #[arg(long, alias = "end", value_name = "YYYY-MM-DD")]
    pub until: Option<String>,
}

impl RangeArgs {
    /// Resolve to `(start, end)` in the portal's `MM/DD/YYYY` form.
    pub fn resolve(&self) -> Result<(String, String), CliError> {
        let (since, until) = self.civil_bounds()?;
        Ok((
            dates::fmt_mm_slash_dd_yyyy(since),
            dates::fmt_mm_slash_dd_yyyy(until),
        ))
    }

    /// The end date alone, for "as of" style endpoints.
    pub fn resolve_end(&self) -> Result<String, CliError> {
        let (_, until) = self.civil_bounds()?;
        Ok(dates::fmt_mm_slash_dd_yyyy(until))
    }

    /// Both bounds as civil dates, defaulted and validated.
    fn civil_bounds(&self) -> Result<(dates::Civil, dates::Civil), CliError> {
        let today = dates::today();
        let since = match &self.since {
            Some(s) => dates::parse_iso(s)?,
            None => (today.0, 1, 1),
        };
        let until = match &self.until {
            Some(s) => dates::parse_iso(s)?,
            None => today,
        };
        if since > until {
            return Err(CliError::Usage(format!(
                "--since {} is after --until {}",
                dates::fmt_iso(since),
                dates::fmt_iso(until)
            )));
        }
        Ok((since, until))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(since: Option<&str>, until: Option<&str>) -> RangeArgs {
        RangeArgs {
            since: since.map(String::from),
            until: until.map(String::from),
        }
    }

    #[test]
    fn resolves_to_the_portal_date_format() {
        let (s, e) = range(Some("2026-01-31"), Some("2026-12-05"))
            .resolve()
            .unwrap();
        assert_eq!(s, "01/31/2026");
        assert_eq!(e, "12/05/2026");
    }

    #[test]
    fn defaults_to_year_to_date() {
        let (start, end) = range(None, None).resolve().unwrap();
        let today = dates::today();
        assert_eq!(start, format!("01/01/{}", today.0));
        assert_eq!(end, dates::fmt_mm_slash_dd_yyyy(today));
    }

    #[test]
    fn resolve_end_defaults_to_today() {
        assert_eq!(
            range(None, None).resolve_end().unwrap(),
            dates::fmt_mm_slash_dd_yyyy(dates::today())
        );
        assert_eq!(
            range(None, Some("2026-03-04")).resolve_end().unwrap(),
            "03/04/2026"
        );
    }

    #[test]
    fn bad_dates_are_usage_errors() {
        for bad in [
            "01/31/2026",
            "not-a-date",
            "2026-01",
            "",
            "2026-13-01",
            "2026-01-32",
        ] {
            assert!(
                range(Some(bad), None).resolve().is_err(),
                "{bad} should be rejected"
            );
            assert!(
                range(None, Some(bad)).resolve_end().is_err(),
                "{bad} should be rejected as an end date"
            );
        }
    }

    /// `pk_cli_core::dates::parse_iso` accepts single-digit months and days;
    /// the portal only ever sees the zero-padded form we emit, so there's no
    /// reason to be stricter than the rest of the family here.
    #[test]
    fn loose_iso_is_accepted_and_normalized() {
        let (s, e) = range(Some("2026-1-5"), Some("2026-3-9")).resolve().unwrap();
        assert_eq!(s, "01/05/2026");
        assert_eq!(e, "03/09/2026");
    }

    #[test]
    fn inverted_range_is_rejected() {
        let err = range(Some("2026-06-01"), Some("2026-01-01"))
            .resolve()
            .unwrap_err();
        assert!(matches!(err, CliError::Usage(_)));
    }
}
