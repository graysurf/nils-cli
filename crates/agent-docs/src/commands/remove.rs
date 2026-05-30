//! `remove` — delete a `[[document]]` entry from the project catalog, matched
//! by `context` + `scope` + `path`. Formatting and surrounding entries are
//! preserved via `toml_edit`.

use std::fs;
use std::io;
use std::path::PathBuf;

use toml_edit::DocumentMut;

use crate::config::config_path_for_root;
use crate::env::ResolvedRoots;
use crate::model::{RemoveOutcome, RemoveReport, Scope};

#[derive(Debug)]
pub enum RemoveError {
    Io(io::Error),
    Parse(String),
}

impl std::fmt::Display for RemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Parse(message) => write!(f, "{message}"),
        }
    }
}

pub struct RemoveRequest {
    pub context: String,
    pub scope: Scope,
    pub path: PathBuf,
}

pub fn run_remove(
    roots: &ResolvedRoots,
    request: RemoveRequest,
) -> Result<RemoveReport, RemoveError> {
    let config_path = config_path_for_root(&roots.project_path);
    let path_str = request.path.to_string_lossy().to_string();

    if !config_path.exists() {
        return Ok(RemoveReport {
            config_path,
            outcome: RemoveOutcome::NotFound,
            context: request.context,
            scope: request.scope,
            path: request.path,
            remaining_documents: 0,
        });
    }

    let raw = fs::read_to_string(&config_path).map_err(RemoveError::Io)?;
    let mut doc: DocumentMut = raw.parse().map_err(|err| {
        RemoveError::Parse(format!("invalid TOML in {}: {err}", config_path.display()))
    })?;

    let mut removed = false;
    if let Some(array) = doc
        .get_mut("document")
        .and_then(|item| item.as_array_of_tables_mut())
    {
        let mut remove_index = None;
        for (index, table) in array.iter().enumerate() {
            let matches = table.get("context").and_then(|v| v.as_str())
                == Some(request.context.as_str())
                && table.get("scope").and_then(|v| v.as_str()) == Some(request.scope.as_str())
                && table.get("path").and_then(|v| v.as_str()) == Some(path_str.as_str());
            if matches {
                remove_index = Some(index);
                break;
            }
        }
        if let Some(index) = remove_index {
            array.remove(index);
            removed = true;
        }
    }

    let remaining_documents = doc
        .get("document")
        .and_then(|item| item.as_array_of_tables())
        .map(|array| array.len())
        .unwrap_or(0);

    if removed {
        fs::write(&config_path, doc.to_string()).map_err(RemoveError::Io)?;
    }

    Ok(RemoveReport {
        config_path,
        outcome: if removed {
            RemoveOutcome::Removed
        } else {
            RemoveOutcome::NotFound
        },
        context: request.context,
        scope: request.scope,
        path: request.path,
        remaining_documents,
    })
}
