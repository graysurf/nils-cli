use rusqlite::{Connection, params};
use serde::Serialize;

use crate::errors::AppError;

use super::repository::QueryState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    Raw,
    Derived,
    Tags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMatchMode {
    Fts,
    Prefix,
    Contains,
}

impl SearchField {
    fn fts_column(self) -> &'static str {
        match self {
            Self::Raw => "raw_text",
            Self::Derived => "derived_text",
            Self::Tags => "tags_text",
        }
    }

    pub fn label(self) -> &'static str {
        self.fts_column()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchItem {
    pub item_id: i64,
    pub created_at: String,
    pub score: f64,
    pub matched_fields: Vec<String>,
    pub preview: String,
    pub content_type: Option<String>,
    pub validation_status: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum ReportPeriod {
    Week,
    Month,
}

#[derive(Debug, Clone)]
pub struct ReportRangeQuery {
    pub period: String,
    pub from: String,
    pub to: String,
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NameCount {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportRange {
    pub from: String,
    pub to: String,
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportTotals {
    pub captured: i64,
    pub enriched: i64,
    pub pending: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportSummary {
    pub period: String,
    pub range: ReportRange,
    pub totals: ReportTotals,
    pub top_categories: Vec<NameCount>,
    pub top_tags: Vec<NameCount>,
    pub top_content_types: Vec<NameCount>,
    pub validation_status_totals: Vec<NameCount>,
}

pub fn search_items(
    conn: &Connection,
    query: &str,
    state: QueryState,
    fields: &[SearchField],
    match_mode: SearchMatchMode,
    limit: usize,
) -> Result<Vec<SearchItem>, AppError> {
    let fields = normalize_search_fields(fields);
    let state_filter = state_filter_sql(state);
    let matched_fields = fields
        .iter()
        .map(|field| field.label().to_string())
        .collect::<Vec<_>>();

    match match_mode {
        SearchMatchMode::Fts => {
            search_items_fts(conn, query, &fields, state_filter, &matched_fields, limit)
        }
        SearchMatchMode::Prefix => {
            let prefix_query = build_prefix_query(query);
            search_items_fts(
                conn,
                &prefix_query,
                &fields,
                state_filter,
                &matched_fields,
                limit,
            )
        }
        SearchMatchMode::Contains => {
            search_items_contains(conn, query, &fields, state_filter, &matched_fields, limit)
        }
    }
}

fn normalize_search_fields(fields: &[SearchField]) -> Vec<SearchField> {
    let mut out = Vec::new();
    let source = if fields.is_empty() {
        &[SearchField::Raw, SearchField::Derived, SearchField::Tags][..]
    } else {
        fields
    };

    for field in source {
        if !out.contains(field) {
            out.push(*field);
        }
    }

    out
}

fn build_scoped_query(query: &str, fields: &[SearchField]) -> String {
    let columns = fields
        .iter()
        .map(|field| field.fts_column())
        .collect::<Vec<_>>()
        .join(" ");
    format!("{{{columns}}}: ({query})")
}

fn state_filter_sql(state: QueryState) -> &'static str {
    match state {
        QueryState::All => "1 = 1",
        QueryState::Pending => {
            "not exists (
                select 1 from item_derivations d
                where d.item_id = i.item_id and d.is_active = 1 and d.status = 'accepted'
            )"
        }
        QueryState::Enriched => {
            "exists (
                select 1 from item_derivations d
                where d.item_id = i.item_id and d.is_active = 1 and d.status = 'accepted'
            )"
        }
    }
}

fn search_items_fts(
    conn: &Connection,
    query: &str,
    fields: &[SearchField],
    state_filter: &str,
    matched_fields: &[String],
    limit: usize,
) -> Result<Vec<SearchItem>, AppError> {
    let scoped_query = build_scoped_query(query, fields);
    let sql = format!(
        "select
            i.item_id,
            i.created_at,
            bm25(item_search_fts) as score,
            substr(coalesce(doc.derived_text, i.raw_text), 1, 120) as preview,
            json_extract(ad.payload_json, '$.content_type') as content_type,
            json_extract(ad.payload_json, '$.validation_status') as validation_status
        from item_search_fts
        join item_search_documents doc on doc.item_id = item_search_fts.rowid
        join inbox_items i on i.item_id = doc.item_id
        left join item_derivations ad
          on ad.derivation_id = (
            select d.derivation_id
            from item_derivations d
            where d.item_id = i.item_id
              and d.is_active = 1
              and d.status = 'accepted'
            order by d.derivation_version desc, d.derivation_id desc
            limit 1
          )
        where item_search_fts match ?1
          and {state_filter}
        order by score asc, i.created_at desc, i.item_id desc
        limit ?2"
    );
    let mut stmt = conn.prepare(&sql).map_err(AppError::db_query)?;
    let rows = stmt
        .query_map(params![scoped_query, limit as i64], |row| {
            Ok(SearchItem {
                item_id: row.get(0)?,
                created_at: row.get(1)?,
                score: row.get(2)?,
                matched_fields: matched_fields.to_vec(),
                preview: row.get(3)?,
                content_type: row.get(4)?,
                validation_status: row.get(5)?,
            })
        })
        .map_err(AppError::db_query)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::db_query)
}

fn search_items_contains(
    conn: &Connection,
    query: &str,
    fields: &[SearchField],
    state_filter: &str,
    matched_fields: &[String],
    limit: usize,
) -> Result<Vec<SearchItem>, AppError> {
    let contains_filter = build_contains_filter(fields);
    let sql = format!(
        "select
            i.item_id,
            i.created_at,
            0.0 as score,
            substr(coalesce(doc.derived_text, i.raw_text), 1, 120) as preview,
            json_extract(ad.payload_json, '$.content_type') as content_type,
            json_extract(ad.payload_json, '$.validation_status') as validation_status
        from item_search_documents doc
        join inbox_items i on i.item_id = doc.item_id
        left join item_derivations ad
          on ad.derivation_id = (
            select d.derivation_id
            from item_derivations d
            where d.item_id = i.item_id
              and d.is_active = 1
              and d.status = 'accepted'
            order by d.derivation_version desc, d.derivation_id desc
            limit 1
          )
        where ({contains_filter})
          and {state_filter}
        order by i.created_at desc, i.item_id desc
        limit ?2"
    );
    let mut stmt = conn.prepare(&sql).map_err(AppError::db_query)?;
    let rows = stmt
        .query_map(params![query, limit as i64], |row| {
            Ok(SearchItem {
                item_id: row.get(0)?,
                created_at: row.get(1)?,
                score: row.get(2)?,
                matched_fields: matched_fields.to_vec(),
                preview: row.get(3)?,
                content_type: row.get(4)?,
                validation_status: row.get(5)?,
            })
        })
        .map_err(AppError::db_query)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::db_query)
}

fn build_contains_filter(fields: &[SearchField]) -> String {
    fields
        .iter()
        .map(|field| {
            format!(
                "instr(lower(coalesce(doc.{column}, '')), lower(?1)) > 0",
                column = field.fts_column()
            )
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

fn build_prefix_query(query: &str) -> String {
    let tokens = query
        .split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(prefix_token)
        .collect::<Vec<_>>();

    if tokens.is_empty() {
        query.to_string()
    } else {
        tokens.join(" ")
    }
}

fn prefix_token(token: &str) -> String {
    let token = token.trim_end_matches('*');
    let escaped = token.replace('"', "\"\"");
    format!("\"{escaped}\"*")
}

pub fn report_summary(conn: &Connection, period: ReportPeriod) -> Result<ReportSummary, AppError> {
    let (period_name, from_sql, to_sql) = match period {
        ReportPeriod::Week => (
            "week",
            "strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-7 days')",
            "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        ),
        ReportPeriod::Month => (
            "month",
            "strftime('%Y-%m-%dT%H:%M:%fZ', 'now', 'start of month')",
            "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        ),
    };

    let (from, to): (String, String) = conn
        .query_row(&format!("select {from_sql}, {to_sql}"), [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(AppError::db_query)?;

    let query = ReportRangeQuery {
        period: period_name.to_string(),
        from,
        to,
        timezone: "UTC".to_string(),
    };
    report_summary_with_range(conn, &query)
}

pub fn report_summary_with_range(
    conn: &Connection,
    query: &ReportRangeQuery,
) -> Result<ReportSummary, AppError> {
    let from = &query.from;
    let to = &query.to;

    let captured: i64 = conn
        .query_row(
            "select count(*)
             from inbox_items
             where julianday(created_at) >= julianday(?1)
               and julianday(created_at) <= julianday(?2)",
            params![from, to],
            |row| row.get(0),
        )
        .map_err(AppError::db_query)?;

    let enriched: i64 = conn
        .query_row(
            "select count(distinct i.item_id)
             from inbox_items i
             join item_derivations d on d.item_id = i.item_id
             where d.is_active = 1
               and d.status = 'accepted'
               and julianday(i.created_at) >= julianday(?1)
               and julianday(i.created_at) <= julianday(?2)",
            params![from, to],
            |row| row.get(0),
        )
        .map_err(AppError::db_query)?;

    let pending = (captured - enriched).max(0);
    let top_categories = collect_top_categories(conn, from, to)?;
    let top_tags = collect_top_tags(conn, from, to)?;
    let top_content_types = collect_top_content_types(conn, from, to)?;
    let validation_status_totals = collect_validation_status_totals(conn, from, to)?;

    Ok(ReportSummary {
        period: query.period.clone(),
        range: ReportRange {
            from: from.clone(),
            to: to.clone(),
            timezone: query.timezone.clone(),
        },
        totals: ReportTotals {
            captured,
            enriched,
            pending,
        },
        top_categories,
        top_tags,
        top_content_types,
        validation_status_totals,
    })
}

fn collect_top_categories(
    conn: &Connection,
    from: &str,
    to: &str,
) -> Result<Vec<NameCount>, AppError> {
    let mut stmt = conn
        .prepare(
            "select coalesce(nullif(trim(d.category), ''), 'uncategorized') as category_name,
                    count(*) as category_count
             from item_derivations d
             join inbox_items i on i.item_id = d.item_id
             where d.is_active = 1
               and d.status = 'accepted'
               and julianday(i.created_at) >= julianday(?1)
               and julianday(i.created_at) <= julianday(?2)
             group by category_name
             order by category_count desc, category_name asc
             limit 5",
        )
        .map_err(AppError::db_query)?;

    let rows = stmt
        .query_map(params![from, to], |row| {
            Ok(NameCount {
                name: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .map_err(AppError::db_query)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::db_query)
}

fn collect_top_tags(conn: &Connection, from: &str, to: &str) -> Result<Vec<NameCount>, AppError> {
    let mut stmt = conn
        .prepare(
            "select t.tag_name, count(*) as tag_count
             from item_tags it
             join tags t on t.tag_id = it.tag_id
             join item_derivations d on d.derivation_id = it.derivation_id
             join inbox_items i on i.item_id = d.item_id
             where d.is_active = 1
               and d.status = 'accepted'
               and julianday(i.created_at) >= julianday(?1)
               and julianday(i.created_at) <= julianday(?2)
             group by t.tag_name
             order by tag_count desc, t.tag_name asc
             limit 5",
        )
        .map_err(AppError::db_query)?;

    let rows = stmt
        .query_map(params![from, to], |row| {
            Ok(NameCount {
                name: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .map_err(AppError::db_query)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::db_query)
}

fn collect_top_content_types(
    conn: &Connection,
    from: &str,
    to: &str,
) -> Result<Vec<NameCount>, AppError> {
    let mut stmt = conn
        .prepare(
            "select coalesce(nullif(trim(json_extract(d.payload_json, '$.content_type')), ''), 'unknown') as content_type,
                    count(*) as content_type_count
             from item_derivations d
             join inbox_items i on i.item_id = d.item_id
             where d.is_active = 1
               and d.status = 'accepted'
               and julianday(i.created_at) >= julianday(?1)
               and julianday(i.created_at) <= julianday(?2)
             group by content_type
             order by content_type_count desc, content_type asc
             limit 7",
        )
        .map_err(AppError::db_query)?;

    let rows = stmt
        .query_map(params![from, to], |row| {
            Ok(NameCount {
                name: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .map_err(AppError::db_query)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::db_query)
}

fn collect_validation_status_totals(
    conn: &Connection,
    from: &str,
    to: &str,
) -> Result<Vec<NameCount>, AppError> {
    let mut stmt = conn
        .prepare(
            "select coalesce(nullif(trim(json_extract(d.payload_json, '$.validation_status')), ''), 'unknown') as validation_status,
                    count(*) as validation_status_count
             from item_derivations d
             join inbox_items i on i.item_id = d.item_id
             where d.is_active = 1
               and d.status = 'accepted'
               and julianday(i.created_at) >= julianday(?1)
               and julianday(i.created_at) <= julianday(?2)
             group by validation_status
             order by validation_status_count desc, validation_status asc",
        )
        .map_err(AppError::db_query)?;

    let rows = stmt
        .query_map(params![from, to], |row| {
            Ok(NameCount {
                name: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .map_err(AppError::db_query)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(AppError::db_query)
}
