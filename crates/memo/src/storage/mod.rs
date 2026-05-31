pub mod derivations;
pub mod migrate;
pub mod repository;
pub mod search;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, Transaction};

use crate::errors::AppError;

#[derive(Debug, Clone)]
pub struct Storage {
    db_path: PathBuf,
}

impl Storage {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    pub fn init(&self) -> Result<(), AppError> {
        let _conn = self.open_connection()?;
        Ok(())
    }

    pub fn with_connection<T, F>(&self, f: F) -> Result<T, AppError>
    where
        F: FnOnce(&Connection) -> Result<T, AppError>,
    {
        let conn = self.open_connection()?;
        f(&conn)
    }

    pub fn with_transaction<T, F>(&self, f: F) -> Result<T, AppError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, AppError>,
    {
        let mut conn = self.open_connection()?;
        let tx = conn.transaction().map_err(AppError::db_write)?;
        let out = f(&tx)?;
        tx.commit().map_err(AppError::db_write)?;
        Ok(out)
    }

    fn open_connection(&self) -> Result<Connection, AppError> {
        if let Some(parent) = self.db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(AppError::db_open)?;
        }

        let conn = Connection::open(&self.db_path).map_err(AppError::db_open)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(AppError::db_open)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(AppError::db_open)?;
        conn.busy_timeout(Duration::from_secs(2))
            .map_err(AppError::db_open)?;

        migrate::apply(&conn)?;
        Ok(conn)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::Storage;

    fn test_db_path(name: &str) -> PathBuf {
        let dir = tempfile::tempdir().expect("tempdir should be created");
        dir.keep().join(format!("{name}.db"))
    }

    #[test]
    fn init_db() {
        let db_path = test_db_path("init_db");
        let storage = Storage::new(db_path);
        storage.init().expect("storage init should succeed");

        let table_name: String = storage
            .with_connection(|conn| {
                conn.query_row(
                    "select name from sqlite_master where type='table' and name='inbox_items'",
                    [],
                    |row| row.get(0),
                )
                .map_err(crate::errors::AppError::db_query)
            })
            .expect("inbox_items table should exist");

        assert_eq!(table_name, "inbox_items");
    }

    #[test]
    fn migration_idempotent() {
        let db_path = test_db_path("migration_idempotent");
        let storage = Storage::new(db_path);
        storage.init().expect("first init should succeed");
        storage.init().expect("second init should succeed");

        let applied_count_v1: i64 = storage
            .with_connection(|conn| {
                conn.query_row(
                    "select count(*) from schema_migrations where version = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(crate::errors::AppError::db_query)
            })
            .expect("schema migration count query should succeed");
        let applied_count_total: i64 = storage
            .with_connection(|conn| {
                conn.query_row("select count(*) from schema_migrations", [], |row| {
                    row.get(0)
                })
                .map_err(crate::errors::AppError::db_query)
            })
            .expect("schema migration count query should succeed");

        assert_eq!(applied_count_v1, 1);
        assert_eq!(applied_count_total, 1);
    }
}
