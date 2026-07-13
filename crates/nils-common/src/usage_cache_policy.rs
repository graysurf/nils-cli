//! Provider-neutral policy for deciding whether cached usage data may be displayed.

/// Cached usage data is never display-eligible at or beyond this age.
pub const MAX_DISPLAY_AGE_SECONDS: i64 = 600;

/// Small future timestamp offsets tolerated as provider/local clock skew.
pub const FUTURE_CLOCK_TOLERANCE_SECONDS: i64 = 5;

/// Classification of a cached usage timestamp relative to the caller's clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsageCacheEligibility {
    Eligible,
    MissingOrInvalidTimestamp,
    Expired,
    FutureTimestampBeyondTolerance,
}

impl UsageCacheEligibility {
    /// Returns whether cached usage values may be displayed.
    pub const fn is_display_eligible(self) -> bool {
        matches!(self, Self::Eligible)
    }
}

/// Classifies a signed cache age supplied by a provider adapter.
///
/// Positive values are seconds in the past. Negative values represent a
/// timestamp in the future. Callers map missing or invalid provider timestamps
/// to `None` before delegating here.
pub const fn classify_display_age_seconds(age_seconds: Option<i64>) -> UsageCacheEligibility {
    let Some(age_seconds) = age_seconds else {
        return UsageCacheEligibility::MissingOrInvalidTimestamp;
    };

    if age_seconds < -FUTURE_CLOCK_TOLERANCE_SECONDS {
        UsageCacheEligibility::FutureTimestampBeyondTolerance
    } else if age_seconds >= MAX_DISPLAY_AGE_SECONDS {
        UsageCacheEligibility::Expired
    } else {
        UsageCacheEligibility::Eligible
    }
}

#[cfg(test)]
mod tests {
    use super::{UsageCacheEligibility, classify_display_age_seconds};
    use pretty_assertions::assert_eq;

    #[test]
    fn classify_display_age_covers_contract_boundaries() {
        let cases = [
            (
                "missing or invalid timestamp",
                None,
                UsageCacheEligibility::MissingOrInvalidTimestamp,
            ),
            (
                "future by six seconds",
                Some(-6),
                UsageCacheEligibility::FutureTimestampBeyondTolerance,
            ),
            (
                "largest future offset",
                Some(i64::MIN),
                UsageCacheEligibility::FutureTimestampBeyondTolerance,
            ),
            (
                "future by five seconds",
                Some(-5),
                UsageCacheEligibility::Eligible,
            ),
            ("current", Some(0), UsageCacheEligibility::Eligible),
            (
                "past by 599 seconds",
                Some(599),
                UsageCacheEligibility::Eligible,
            ),
            (
                "past by 600 seconds",
                Some(600),
                UsageCacheEligibility::Expired,
            ),
            (
                "largest past offset",
                Some(i64::MAX),
                UsageCacheEligibility::Expired,
            ),
        ];

        for (label, age_seconds, expected) in cases {
            let actual = classify_display_age_seconds(age_seconds);
            assert_eq!(actual, expected, "{label}");
            assert_eq!(
                actual.is_display_eligible(),
                expected == UsageCacheEligibility::Eligible,
                "{label} display eligibility"
            );
        }
    }
}
