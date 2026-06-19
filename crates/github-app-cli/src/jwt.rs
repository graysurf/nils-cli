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

    // Throwaway 2048-bit RSA key, generated for this test only — NOT a secret.
    const TEST_RSA_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCsIFTygiKJKmLi
8fiuMSfgMvscehGLWx+e2AEykeODooFAgvU24OYcontzC2Sx9OGX83hXb2P8Fwy7
zXn6B4QhO6DDKfUG9xbXUVz4lXz6mOavt/3Y33XgvixD+0StWgjvA1VUMn0a489C
y1rtMTeM5ueaud4aayfwuY09D9d0cQdX2Q9gfcHIVUQ/pFLt2hO2WRGEQNRdC0V7
jv6ENop6fWSlnO7Ut8XVmFixFAsLn4mgEg4LYE57/8XjBhLvg9FOuryU0FNQHR3K
LxQWaaQfjYzB3ZPwZL3CvVM9lsH0tVnMOLcBLUGpyhzhqMeQFdS3BNIFf9EkXJbf
osbB6McXAgMBAAECggEACvlywRWIAyvXKzYXT2/l5XcKqKmlxbdVIEkQZnuDwIhT
alwPK2USdt/rNA4VaP0+hvQoh5acDt4fWzgCH21sQLwvB1J9A2kspSTUYysQ0V9/
UdPO1Q2GVAJ8CweRvOXBLRAO2DPx4w2EUPNrRDU/n/W27ZgNL60GWmRSO4Lvj0Zs
mmDKEtpTYo+M1OQedI7eu5f5SkC6DN85un4uR7u/OxJI7UplxsfeATPxiDQTF32W
VW00XulbkEzUfIfNmuJRW5ox68GviYCYcXBjO2g3sNUGcJrd2xHdwOXJPuhwLfuF
LctQW1HbalScV3aHR9bvM1EpExCcm2FqdpGxjFZSSQKBgQDv2Tt8KSPT0sNi3UBD
Pxscp7IxoO24EEkAXPrELT5CNvxv83CNVsTWKnU73XJVf8JrQgKuaubLCdcljBV9
xnPu7+oNSIQcjE+LvrXrm3fZOoryvYH1fz0X04fvmQeTeVw6sZcooZByMvPBvlUQ
RUKA4yDdGKqt4NIFPWEdFNjqLwKBgQC3t6FQtAilRitT5OP2pQPuyd48SzsTYSyX
S1651uwXsNzTBpP6P3LnRVD0sD54hXsqQI7haWnFBYKMGYiTR8LT0xhFzQqsBDdR
pvI1krgDQZm6azRpPIOHAnGTBSoeMJ87BaXdvEOnalZkM2rUN8oLDxT5541ifaaT
t8BStQD/mQKBgCFJDWdKsk0oN7NVryBl9pZAc4tNoQ/lOqxROv/Uo4o+5UOIDjuf
KgvqsoBPWBmjdFC8RXD9JvBQekocqbLdwqMLKnkTcjogAr4LBmYfGj/MTxIm2I1A
TjMrSPcoTpPZyMHgeXDLEye2CHv/tQBgDD2kx5/HV5Bv3dWaUgreJMhDAoGAFE9D
kRVmA0dfkNWz8ddKOQKuA8JZVIogkNUvMqI01WWi8909TKPpAvIhwfsd3Nr8w64B
XZ/2pmY2iWBlPcroGdyzSTwimuOYbflju1Jt70Y4RWiGkb+z1qAJiDRA9LdxUugL
7xhZ7k8OH+OjQrSsLE7Nhdb4RVQYrynYJAyIgLkCgYBheF7YKo7OAhj2N7V11C1y
5XREj1ihA5B1yBghfTm38W0OvzyBt8hUWXbs9drm82nnC5u/4unBoeBU7nk6fejq
Zrc1JqeyWh2Cew8qf7OrCtauwdJKY/glsfFU5EOWLoeBCq8iuyYvOn1CundezUja
ran2RV5nH6M4OMqtZjoNgg==
-----END PRIVATE KEY-----
";

    // Regression: jsonwebtoken 10 splits its crypto backend into mutually
    // exclusive `rust_crypto` / `aws_lc_rs` features and PANICS at runtime when
    // signing if neither is selected under `default-features = false` (the
    // v1.11.0 state that broke `github-app-cli token`). This signs a real
    // RS256 JWT, so it fails closed if a future dependency change drops the
    // crypto backend feature again.
    #[test]
    fn app_jwt_signs_rs256_without_crypto_provider_panic() {
        let token = app_jwt("4090218", TEST_RSA_PEM.as_bytes(), 1_000_000)
            .expect("RS256 signing must succeed with a crypto backend enabled");
        assert_eq!(
            token.split('.').count(),
            3,
            "a JWT must have header.payload.signature"
        );
        assert!(
            !token.split('.').nth(2).unwrap().is_empty(),
            "the signature segment must be non-empty"
        );
    }
}
