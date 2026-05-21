use serde_json::json;

use crate::cli::{OutputFormat, SearchField as CliSearchField};
use crate::errors::AppError;
use crate::output::{emit_data, format_item_id, text};
use crate::storage::Storage;
use crate::storage::repository::QueryState;
use crate::storage::search;

pub fn run(
    storage: &Storage,
    format: OutputFormat,
    state: QueryState,
    query: &str,
    fields: &[CliSearchField],
    match_mode: search::SearchMatchMode,
    limit: usize,
) -> Result<(), AppError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(AppError::usage("search requires a non-empty query"));
    }

    let search_fields = map_search_fields(fields);
    let rows = storage.with_connection(|conn| {
        search::search_items(conn, query, state, &search_fields, match_mode, limit)
    })?;
    let truncated = limit > 0 && rows.len() == limit;

    if format.is_json() {
        let items = rows
            .iter()
            .map(|row| {
                json!({
                    "item_id": format_item_id(row.item_id),
                    "created_at": row.created_at,
                    "score": row.score,
                    "matched_fields": row.matched_fields,
                    "preview": row.preview,
                    "content_type": row.content_type,
                    "validation_status": row.validation_status,
                })
            })
            .collect::<Vec<_>>();
        return emit_data(
            "cli.memo-cli.search.v1",
            json!({
                "items": items,
                "meta": {
                    "query": query,
                    "limit": limit,
                    "state": query_state_label(state),
                    "fields": search_field_labels(&search_fields),
                    "match": search_match_mode_label(match_mode),
                    "truncated": truncated,
                },
            }),
        );
    }

    text::print_search(&rows, limit, truncated);

    Ok(())
}

fn query_state_label(state: QueryState) -> &'static str {
    match state {
        QueryState::All => "all",
        QueryState::Pending => "pending",
        QueryState::Enriched => "enriched",
    }
}

fn map_search_fields(fields: &[CliSearchField]) -> Vec<search::SearchField> {
    let mut out = Vec::new();
    let source = if fields.is_empty() {
        &[
            CliSearchField::Raw,
            CliSearchField::Derived,
            CliSearchField::Tags,
        ][..]
    } else {
        fields
    };

    for field in source {
        let mapped = match field {
            CliSearchField::Raw => search::SearchField::Raw,
            CliSearchField::Derived => search::SearchField::Derived,
            CliSearchField::Tags => search::SearchField::Tags,
        };
        if !out.contains(&mapped) {
            out.push(mapped);
        }
    }

    out
}

fn search_field_labels(fields: &[search::SearchField]) -> Vec<&'static str> {
    fields.iter().map(|field| field.label()).collect()
}

fn search_match_mode_label(mode: search::SearchMatchMode) -> &'static str {
    match mode {
        search::SearchMatchMode::Fts => "fts",
        search::SearchMatchMode::Prefix => "prefix",
        search::SearchMatchMode::Contains => "contains",
    }
}
