//! Provider-neutral usage failure reasons shared by usage helpers.
//!
//! This module owns stable machine-readable classification only. Provider- and
//! product-specific user-facing messages stay in caller adapters.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderUsageReason {
    AuthRequired,
    AuthExpired,
    BillingPastDue,
    SubscriptionInactive,
    OrganizationDisabled,
    PermissionDenied,
    RateLimited,
    ServiceUnavailable,
    Timeout,
    Unknown,
}

impl ProviderUsageReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthRequired => "auth_required",
            Self::AuthExpired => "auth_expired",
            Self::BillingPastDue => "billing_past_due",
            Self::SubscriptionInactive => "subscription_inactive",
            Self::OrganizationDisabled => "organization_disabled",
            Self::PermissionDenied => "permission_denied",
            Self::RateLimited => "rate_limited",
            Self::ServiceUnavailable => "service_unavailable",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_code(value: &str) -> Option<Self> {
        match value {
            "auth_required" => Some(Self::AuthRequired),
            "auth_expired" => Some(Self::AuthExpired),
            "billing_past_due" => Some(Self::BillingPastDue),
            "subscription_inactive" => Some(Self::SubscriptionInactive),
            "organization_disabled" => Some(Self::OrganizationDisabled),
            "permission_denied" => Some(Self::PermissionDenied),
            "rate_limited" => Some(Self::RateLimited),
            "service_unavailable" => Some(Self::ServiceUnavailable),
            "timeout" => Some(Self::Timeout),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    const fn priority(self) -> u8 {
        match self {
            Self::BillingPastDue => 10,
            Self::OrganizationDisabled => 9,
            Self::SubscriptionInactive => 8,
            Self::AuthExpired => 7,
            Self::AuthRequired => 6,
            Self::PermissionDenied => 5,
            Self::RateLimited => 4,
            Self::Timeout => 3,
            Self::ServiceUnavailable => 2,
            Self::Unknown => 1,
        }
    }
}

pub fn classify_http_failure(status: u16, body: &str) -> ProviderUsageReason {
    let classified = classify_message(body);
    if classified != ProviderUsageReason::Unknown {
        return classified;
    }
    match status {
        401 => ProviderUsageReason::AuthExpired,
        402 => ProviderUsageReason::BillingPastDue,
        403 => ProviderUsageReason::PermissionDenied,
        429 => ProviderUsageReason::RateLimited,
        500..=599 => ProviderUsageReason::ServiceUnavailable,
        _ => ProviderUsageReason::Unknown,
    }
}

pub fn classify_message(message: &str) -> ProviderUsageReason {
    let lower = message.to_ascii_lowercase();
    if lower.contains("past due")
        || lower.contains("overdue invoice")
        || lower.contains("payment required")
    {
        return ProviderUsageReason::BillingPastDue;
    }
    if lower.contains("organization has disabled")
        || lower.contains("organization disabled")
        || lower.contains("disabled by your organization")
    {
        return ProviderUsageReason::OrganizationDisabled;
    }
    if (lower.contains("subscription") || lower.contains("plan"))
        && (lower.contains("inactive")
            || lower.contains("disabled")
            || lower.contains("not active")
            || lower.contains("expired"))
    {
        return ProviderUsageReason::SubscriptionInactive;
    }
    if lower.contains("token expired")
        || lower.contains("authentication expired")
        || lower.contains("sign-in expired")
        || lower.contains("login expired")
    {
        return ProviderUsageReason::AuthExpired;
    }
    if lower.contains("not logged in")
        || lower.contains("sign in to")
        || lower.contains("please sign in")
        || lower.contains("please login")
        || lower.contains("authentication required")
    {
        return ProviderUsageReason::AuthRequired;
    }
    if lower.contains("rate limit") || lower.contains("too many requests") {
        return ProviderUsageReason::RateLimited;
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return ProviderUsageReason::Timeout;
    }
    ProviderUsageReason::Unknown
}

pub const fn prefer_reason(
    first: ProviderUsageReason,
    second: ProviderUsageReason,
) -> ProviderUsageReason {
    if second.priority() > first.priority() {
        second
    } else {
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn classifies_actionable_provider_messages_before_status_fallbacks() {
        assert_eq!(
            classify_http_failure(
                403,
                "Your organization has disabled Claude subscription access"
            ),
            ProviderUsageReason::OrganizationDisabled
        );
        assert_eq!(
            classify_http_failure(403, "Your subscription payment is past due"),
            ProviderUsageReason::BillingPastDue
        );
        assert_eq!(
            classify_http_failure(401, ""),
            ProviderUsageReason::AuthExpired
        );
        assert_eq!(
            classify_http_failure(429, ""),
            ProviderUsageReason::RateLimited
        );
    }

    #[test]
    fn rejects_unknown_codes_and_prefers_actionable_reasons() {
        assert_eq!(ProviderUsageReason::from_code("future"), None);
        assert_eq!(
            prefer_reason(
                ProviderUsageReason::Timeout,
                ProviderUsageReason::OrganizationDisabled
            ),
            ProviderUsageReason::OrganizationDisabled
        );
    }
}
