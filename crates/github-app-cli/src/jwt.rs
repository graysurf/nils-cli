//! GitHub App JWT minting (RS256).
//!
//! See <https://docs.github.com/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app>.

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;

/// Backdate `iat` to tolerate small clock skew between this host and GitHub.
const CLOCK_SKEW_SECS: u64 = 60;
/// JWT lifetime. GitHub rejects App JWTs valid for more than 10 minutes; use 9
/// to stay comfortably inside the limit.
const LIFETIME_SECS: u64 = 9 * 60;

/// Claims for a GitHub App JWT.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Claims {
    /// Issued-at (Unix seconds), backdated by [`CLOCK_SKEW_SECS`].
    pub iat: u64,
    /// Expiry (Unix seconds).
    pub exp: u64,
    /// Issuer: the GitHub App ID or Client ID.
    pub iss: String,
}

impl Claims {
    /// Build App JWT claims for `app_id` at `now_secs` (Unix seconds).
    pub fn new(app_id: &str, now_secs: u64) -> Self {
        Self {
            iat: now_secs.saturating_sub(CLOCK_SKEW_SECS),
            exp: now_secs + LIFETIME_SECS,
            iss: app_id.to_string(),
        }
    }
}

/// Sign a GitHub App JWT (RS256) for `app_id` with the RSA private key `pem`.
pub fn app_jwt(
    app_id: &str,
    pem: &[u8],
    now_secs: u64,
) -> Result<String, jsonwebtoken::errors::Error> {
    let claims = Claims::new(app_id, now_secs);
    let key = EncodingKey::from_rsa_pem(pem)?;
    encode(&Header::new(Algorithm::RS256), &claims, &key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn claims_backdate_iat_and_set_nine_minute_expiry() {
        let claims = Claims::new("4090218", 1_000_000);
        assert_eq!(claims.iat, 1_000_000 - 60);
        assert_eq!(claims.exp, 1_000_000 + 540);
        assert_eq!(claims.iss, "4090218");
    }

    #[test]
    fn claims_iat_saturates_near_epoch() {
        let claims = Claims::new("1", 10);
        assert_eq!(claims.iat, 0);
    }
}
