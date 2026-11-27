//! SQLite persistence for Docket's append-only decision ledger.
//!
//! The database is stored as `docket.sqlite3` under the configured data
//! directory. Item JSON remains opaque to this adapter; insertion metadata
//! is private storage state and is never projected into `docket_core::Item`.

use docket_core::types::validate_item;
use docket_core::{ActionDescriptor, DecisionLogEntry, Item, ItemStatus, ItemStore, StoreError};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// Database file created inside the configured data directory.
pub const DATABASE_FILE_NAME: &str = "docket.sqlite3";

/// Durable SQLite implementation of [`ItemStore`].
pub struct SqliteStore {
    connection: Mutex<Connection>,
}

impl SqliteStore {
    /// Open or initialize the ledger database in `data_dir`.
    ///
    /// The directory is created if it does not exist. Callers choose the
    /// configured location; this type has no hard-coded production path.
    ///
    /// # Errors
    /// Returns a backend error if the directory, database, schema, or WAL
    /// configuration cannot be opened.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        std::fs::create_dir_all(data_dir.as_ref())
            .map_err(|error| backend(format!("create data directory: {error}")))?;
        let database_path = data_dir.as_ref().join(DATABASE_FILE_NAME);
        let connection = Connection::open(database_path).map_err(backend)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(backend)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS docket_meta (
                     key TEXT PRIMARY KEY NOT NULL,
                     value INTEGER NOT NULL CHECK (value >= 0)
                 );
                 INSERT OR IGNORE INTO docket_meta (key, value)
                     VALUES ('next_id', 0);
                 CREATE TABLE IF NOT EXISTS items (
                     id TEXT PRIMARY KEY NOT NULL,
                     source TEXT NOT NULL,
                     title TEXT NOT NULL,
                     context TEXT NOT NULL,
                     recommendation TEXT NOT NULL,
                     action_target TEXT NOT NULL,
                     action_operation TEXT NOT NULL,
                     action_params_json TEXT NOT NULL,
                     status TEXT NOT NULL CHECK (
                         status IN ('pending', 'dispatched', 'dismissed')
                     ),
                     created_at TEXT NOT NULL DEFAULT (
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     )
                 );
                 CREATE TABLE IF NOT EXISTS decision_log (
                     seq INTEGER PRIMARY KEY AUTOINCREMENT,
                     item_id TEXT NOT NULL REFERENCES items(id) ON DELETE RESTRICT,
                     at TEXT NOT NULL,
                     actor TEXT NOT NULL,
                     decision TEXT NOT NULL,
                     note TEXT
                 );
                 CREATE TRIGGER IF NOT EXISTS decision_log_no_update
                     BEFORE UPDATE ON decision_log
                     BEGIN
                         SELECT RAISE(ABORT, 'decision_log is append-only');
                     END;
                 CREATE TRIGGER IF NOT EXISTS decision_log_no_delete
                     BEFORE DELETE ON decision_log
                     BEGIN
                         SELECT RAISE(ABORT, 'decision_log is append-only');
                     END;",
            )
            .map_err(backend)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.connection
            .lock()
            .map_err(|_| backend("database connection lock poisoned"))
    }
}

impl ItemStore for SqliteStore {
    fn create(
        &self,
        source: &str,
        title: &str,
        context: &str,
        recommendation: &str,
        action: &ActionDescriptor,
    ) -> Result<Item, StoreError> {
        validate_item(context, recommendation, Some(action))
            .map_err(|error| backend(error.to_string()))?;
        let action_params_json = serde_json::to_string(&action.params).map_err(backend)?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        let current: i64 = transaction
            .query_row(
                "SELECT value FROM docket_meta WHERE key = 'next_id'",
                [],
                |row| row.get(0),
            )
            .map_err(backend)?;
        let next = current
            .checked_add(1)
            .filter(|value| *value > 0)
            .ok_or_else(|| backend("item ID counter exhausted"))?;
        let changed = transaction
            .execute(
                "UPDATE docket_meta SET value = ?1 WHERE key = 'next_id'",
                [next],
            )
            .map_err(backend)?;
        if changed != 1 {
            return Err(backend("item ID counter row is missing"));
        }
        let id = format!("dkt-{next:06}");
        transaction
            .execute(
                "INSERT INTO items (
                     id, source, title, context, recommendation,
                     action_target, action_operation, action_params_json, status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending')",
                params![
                    id,
                    source,
                    title,
                    context,
                    recommendation,
                    action.target,
                    action.operation,
                    action_params_json,
                ],
            )
            .map_err(backend)?;
        transaction.commit().map_err(backend)?;

        Ok(Item {
            id,
            source: source.to_string(),
            title: title.to_string(),
            context: context.to_string(),
            recommendation: recommendation.to_string(),
            action_descriptor: action.clone(),
            status: ItemStatus::Pending,
            decision_log: Vec::new(),
        })
    }

    fn get(&self, id: &str) -> Result<Option<Item>, StoreError> {
        let connection = self.connection()?;
        load_item(&connection, id)
    }

    fn list(
        &self,
        status: Option<ItemStatus>,
        source: Option<&str>,
    ) -> Result<Vec<Item>, StoreError> {
        let connection = self.connection()?;
        let status = status.map(|value| value.as_str().to_string());
        let source = source.map(str::to_string);
        let ids = {
            let mut statement = connection
                .prepare(
                    "SELECT id FROM items
                     WHERE (?1 IS NULL OR status = ?1)
                       AND (?2 IS NULL OR source = ?2)
                     ORDER BY id ASC",
                )
                .map_err(backend)?;
            let rows = statement
                .query_map(params![status, source], |row| row.get::<_, String>(0))
                .map_err(backend)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(backend)?
        };
        ids.iter()
            .map(|id| {
                load_item(&connection, id)?
                    .ok_or_else(|| backend("listed item disappeared while reading the ledger"))
            })
            .collect()
    }

    fn record_decision(
        &self,
        id: &str,
        entry: DecisionLogEntry,
        new_status: ItemStatus,
    ) -> Result<Item, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        let exists: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM items WHERE id = ?1)",
                [id],
                |row| row.get(0),
            )
            .map_err(backend)?;
        if !exists {
            return Err(StoreError::NotFound(id.to_string()));
        }
        transaction
            .execute(
                "INSERT INTO decision_log (item_id, at, actor, decision, note)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, entry.at, entry.actor, entry.decision, entry.note],
            )
            .map_err(backend)?;
        let changed = transaction
            .execute(
                "UPDATE items SET status = ?1 WHERE id = ?2",
                params![new_status.as_str(), id],
            )
            .map_err(backend)?;
        if changed != 1 {
            return Err(StoreError::NotFound(id.to_string()));
        }
        let item =
            load_item(&transaction, id)?.ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        transaction.commit().map_err(backend)?;
        Ok(item)
    }
}

fn load_item(connection: &Connection, id: &str) -> Result<Option<Item>, StoreError> {
    let row = connection
        .query_row(
            "SELECT id, source, title, context, recommendation,
                    action_target, action_operation, action_params_json, status
             FROM items WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )
        .optional()
        .map_err(backend)?;
    let Some((
        id,
        source,
        title,
        context,
        recommendation,
        action_target,
        action_operation,
        action_params_json,
        status,
    )) = row
    else {
        return Ok(None);
    };
    let status = parse_status(&status)?;
    let action_params = serde_json::from_str(&action_params_json).map_err(backend)?;
    let mut statement = connection
        .prepare(
            "SELECT at, actor, decision, note
             FROM decision_log WHERE item_id = ?1 ORDER BY seq ASC",
        )
        .map_err(backend)?;
    let rows = statement
        .query_map([&id], |row| {
            Ok(DecisionLogEntry {
                at: row.get(0)?,
                actor: row.get(1)?,
                decision: row.get(2)?,
                note: row.get(3)?,
            })
        })
        .map_err(backend)?;
    let decision_log = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(backend)?;
    Ok(Some(Item {
        id,
        source,
        title,
        context,
        recommendation,
        action_descriptor: ActionDescriptor {
            target: action_target,
            operation: action_operation,
            params: action_params,
        },
        status,
        decision_log,
    }))
}

fn parse_status(status: &str) -> Result<ItemStatus, StoreError> {
    match status {
        "pending" => Ok(ItemStatus::Pending),
        "dispatched" => Ok(ItemStatus::Dispatched),
        "dismissed" => Ok(ItemStatus::Dismissed),
        other => Err(backend(format!("unknown stored item status: {other}"))),
    }
}

fn backend(error: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use docket_core::InMemoryStore;
    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::tempdir;

    fn descriptor() -> ActionDescriptor {
        ActionDescriptor {
            target: "hermes-coding".into(),
            operation: "inspect_pr".into(),
            params: json!({"opaque": {"array": [1, true, null, "value"]}}),
        }
    }

    fn entry(at: &str, actor: &str, decision: &str) -> DecisionLogEntry {
        DecisionLogEntry {
            at: at.to_string(),
            actor: actor.to_string(),
            decision: decision.to_string(),
            note: Some(format!("note for {decision}")),
        }
    }

    fn assert_store_contract(store: &dyn ItemStore) {
        let first = store
            .create(
                "plane",
                "first",
                "synthetic context",
                "inspect",
                &descriptor(),
            )
            .expect("first item");
        let second = store
            .create(
                "fixture",
                "second",
                "synthetic context",
                "inspect",
                &descriptor(),
            )
            .expect("second item");
        assert_eq!(first.status, ItemStatus::Pending);
        assert_eq!(
            store.get(&first.id).expect("get first"),
            Some(first.clone())
        );
        assert_eq!(
            store
                .list(Some(ItemStatus::Pending), Some("plane"))
                .expect("filtered list"),
            vec![first.clone()]
        );
        assert_eq!(store.list(None, None).expect("all items").len(), 2);

        let decision = entry("2026-11-26T00:00:00Z", "fixture-actor", "approve");
        let updated = store
            .record_decision(&first.id, decision.clone(), ItemStatus::Dispatched)
            .expect("record decision");
        assert_eq!(updated.status, ItemStatus::Dispatched);
        assert_eq!(updated.decision_log, vec![decision]);
        assert_eq!(store.get(&second.id).expect("get second"), Some(second));
        assert!(matches!(
            store.record_decision(
                "dkt-missing",
                entry("2026-11-26T00:00:01Z", "fixture-actor", "dismiss"),
                ItemStatus::Dismissed
            ),
            Err(StoreError::NotFound(id)) if id == "dkt-missing"
        ));
    }

    #[test]
    fn in_memory_store_satisfies_shared_trait_contract() {
        assert_store_contract(&InMemoryStore::new());
    }

    #[test]
    fn sqlite_store_satisfies_shared_trait_contract() {
        let directory = tempdir().expect("isolated data directory");
        let store = SqliteStore::open(directory.path()).expect("open store");
        assert_store_contract(&store);
    }

    #[test]
    fn reopen_preserves_items_private_metadata_decisions_and_counter() {
        let directory = tempdir().expect("isolated data directory");
        let store = SqliteStore::open(directory.path()).expect("open store");
        let item = store
            .create(
                "fixture",
                "persisted item",
                "synthetic context",
                "preserve it",
                &descriptor(),
            )
            .expect("create item");
        assert_eq!(item.id, "dkt-000001");
        let first = entry("2026-11-26T00:00:00Z", "actor-a", "approve");
        let second = entry("2026-11-26T00:00:01Z", "actor-b", "defer");
        store
            .record_decision(&item.id, first.clone(), ItemStatus::Dispatched)
            .expect("first decision");
        let expected = store
            .record_decision(&item.id, second.clone(), ItemStatus::Dismissed)
            .expect("second decision");
        assert_eq!(expected.decision_log, vec![first, second]);

        let database_path = directory.path().join(DATABASE_FILE_NAME);
        let created_at_before: String = Connection::open(&database_path)
            .expect("open inspection connection")
            .query_row(
                "SELECT created_at FROM items WHERE id = ?1",
                [&item.id],
                |row| row.get(0),
            )
            .expect("read private metadata");
        assert!(!created_at_before.is_empty());
        drop(store);

        let reopened = SqliteStore::open(directory.path()).expect("reopen store");
        assert_eq!(
            reopened.get(&item.id).expect("read reopened item"),
            Some(expected)
        );
        let next = reopened
            .create(
                "fixture",
                "next item",
                "synthetic context",
                "preserve order",
                &descriptor(),
            )
            .expect("create after reopen");
        assert_eq!(next.id, "dkt-000002");
        let created_at_after: String = Connection::open(&database_path)
            .expect("open inspection connection")
            .query_row(
                "SELECT created_at FROM items WHERE id = ?1",
                [&item.id],
                |row| row.get(0),
            )
            .expect("read private metadata after reopen");
        assert_eq!(created_at_after, created_at_before);
        let reopened_item = reopened
            .get(&item.id)
            .expect("public item projection")
            .expect("persisted item exists");
        assert_eq!(reopened_item.action_descriptor.params, descriptor().params);
        assert_eq!(reopened_item.decision_log.len(), 2);
    }

    #[test]
    fn invalid_producer_input_does_not_advance_the_durable_counter() {
        let directory = tempdir().expect("isolated data directory");
        let store = SqliteStore::open(directory.path()).expect("open store");
        let error = store
            .create("fixture", "invalid", "  ", "recommendation", &descriptor())
            .expect_err("missing context must be rejected");
        assert!(error.to_string().contains("no injected context"));
        let valid = store
            .create(
                "fixture",
                "valid",
                "synthetic context",
                "recommendation",
                &descriptor(),
            )
            .expect("valid item");
        assert_eq!(valid.id, "dkt-000001");
    }

    #[test]
    fn decision_and_status_roll_back_together_on_transaction_failure() {
        let directory = tempdir().expect("isolated data directory");
        let store = SqliteStore::open(directory.path()).expect("open store");
        let item = store
            .create(
                "fixture",
                "rollback test",
                "synthetic context",
                "preserve atomicity",
                &descriptor(),
            )
            .expect("create item");
        Connection::open(directory.path().join(DATABASE_FILE_NAME))
            .expect("open trigger connection")
            .execute_batch(
                "CREATE TRIGGER fail_status_update
                 BEFORE UPDATE OF status ON items
                 BEGIN
                     SELECT RAISE(ABORT, 'injected transaction failure');
                 END;",
            )
            .expect("install deterministic failure trigger");

        let error = store
            .record_decision(
                &item.id,
                entry("2026-11-26T00:00:00Z", "fixture-actor", "approve"),
                ItemStatus::Dispatched,
            )
            .expect_err("status update trigger must fail");
        assert!(error.to_string().contains("injected transaction failure"));
        let after = store.get(&item.id).expect("read after rollback").unwrap();
        assert_eq!(after.status, ItemStatus::Pending);
        assert!(after.decision_log.is_empty());
    }

    #[test]
    fn decision_log_rows_cannot_be_updated_or_deleted() {
        let directory = tempdir().expect("isolated data directory");
        let store = SqliteStore::open(directory.path()).expect("open store");
        let item = store
            .create(
                "fixture",
                "append-only test",
                "synthetic context",
                "preserve history",
                &descriptor(),
            )
            .expect("create item");
        store
            .record_decision(
                &item.id,
                entry("2026-11-26T00:00:00Z", "fixture-actor", "approve"),
                ItemStatus::Dispatched,
            )
            .expect("append decision");
        let connection = Connection::open(directory.path().join(DATABASE_FILE_NAME))
            .expect("open inspection connection");
        assert!(connection
            .execute(
                "UPDATE decision_log SET decision = 'rewrite' WHERE item_id = ?1",
                [&item.id]
            )
            .is_err());
        assert!(connection
            .execute("DELETE FROM decision_log WHERE item_id = ?1", [&item.id])
            .is_err());
        assert_eq!(
            store
                .get(&item.id)
                .expect("read append-only log")
                .unwrap()
                .decision_log
                .len(),
            1
        );
    }
}
