//! `installations` subcommand payload.

use serde::Serialize;

use crate::github::Installation;

/// One installation row (no secret material).
#[derive(Debug, Clone, Serialize)]
pub struct InstallationRow {
    pub installation_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_selection: Option<String>,
    pub permissions: serde_json::Value,
}

/// JSON payload for the `installations` command.
#[derive(Debug, Clone, Serialize)]
pub struct InstallationsPayload {
    pub installations: Vec<InstallationRow>,
}

impl InstallationsPayload {
    /// Build the payload from the raw API installation list.
    pub fn from_installations(items: &[Installation]) -> Self {
        let installations = items
            .iter()
            .map(|i| InstallationRow {
                installation_id: i.id,
                account: i.account.as_ref().map(|a| a.login.clone()),
                repository_selection: i.repository_selection.clone(),
                permissions: i.permissions.clone(),
            })
            .collect();
        Self { installations }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::Account;
    use pretty_assertions::assert_eq;

    #[test]
    fn maps_account_login_and_selection() {
        let items = vec![Installation {
            id: 141_215_065,
            account: Some(Account {
                login: "graysurf".to_string(),
            }),
            repository_selection: Some("all".to_string()),
            permissions: serde_json::json!({ "issues": "write" }),
        }];
        let payload = InstallationsPayload::from_installations(&items);
        assert_eq!(payload.installations.len(), 1);
        assert_eq!(payload.installations[0].installation_id, 141_215_065);
        assert_eq!(
            payload.installations[0].account.as_deref(),
            Some("graysurf")
        );
    }
}
