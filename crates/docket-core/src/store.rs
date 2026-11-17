//! Storage backends for the ledger. Docket-1 replaces the in-memory
//! store with SQLite under the configured data dir (`/data` volume in
//! deployment); the trait below is the contract both must satisfy.

use crate::types::{ActionDescriptor, DecisionLogEntry, Item, ItemStatus};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Store-level failures (I/O, corruption). Producer-law violations never
/// reach here — [`crate::types::validate_item`] rejects them earlier.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The id does not exist in the ledger.
    #[error("item not found: {0}")]
    NotFound(String),
    /// Backend failure.
    #[error("store failure: {0}")]
    Backend(String),
}

/// The ledger contract. Implementations must keep `decision_log`
/// append-only and assign monotonic `dkt-…` ids.
pub trait ItemStore: Send + Sync {
    /// Validate at the producer boundary and append a new pending item.
    ///
    /// # Errors
    /// Producer-law violations ([`crate::types::ProducerError`]) or
    /// backend failures.
    fn create(
        &self,
        source: &str,
        title: &str,
        context: &str,
        recommendation: &str,
        action: &ActionDescriptor,
    ) -> Result<Item, StoreError>;

    /// Fetch one item by id.
    ///
    /// # Errors
    /// Backend failures.
    fn get(&self, id: &str) -> Result<Option<Item>, StoreError>;

    /// List items, optionally filtered by status and/or source.
    ///
    /// # Errors
    /// Backend failures.
    fn list(
        &self,
        status: Option<ItemStatus>,
        source: Option<&str>,
    ) -> Result<Vec<Item>, StoreError>;

    /// Append a decision to the item's log and move its status forward.
    ///
    /// # Errors
    /// Unknown id or backend failure.
    fn record_decision(
        &self,
        id: &str,
        entry: DecisionLogEntry,
        new_status: ItemStatus,
    ) -> Result<Item, StoreError>;
}

/// Thread-safe in-memory ledger (scaffold + tests). Persistence lands
/// with the SQLite backend (docket-1).
#[derive(Default)]
pub struct InMemoryStore {
    items: Mutex<BTreeMap<String, Item>>,
    counter: Mutex<u64>,
}

impl InMemoryStore {
    /// Empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id(&self) -> String {
        let mut c = self.counter.lock().expect("counter lock");
        *c += 1;
        format!("dkt-{c:06}")
    }
}

impl ItemStore for InMemoryStore {
    fn create(
        &self,
        source: &str,
        title: &str,
        context: &str,
        recommendation: &str,
        action: &ActionDescriptor,
    ) -> Result<Item, StoreError> {
        crate::types::validate_item(context, recommendation, Some(action))
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        let item = Item {
            id: self.next_id(),
            source: source.to_string(),
            title: title.to_string(),
            context: context.to_string(),
            recommendation: recommendation.to_string(),
            action_descriptor: action.clone(),
            status: ItemStatus::Pending,
            decision_log: Vec::new(),
        };
        self.items
            .lock()
            .expect("items lock")
            .insert(item.id.clone(), item.clone());
        Ok(item)
    }

    fn get(&self, id: &str) -> Result<Option<Item>, StoreError> {
        Ok(self.items.lock().expect("items lock").get(id).cloned())
    }

    fn list(
        &self,
        status: Option<ItemStatus>,
        source: Option<&str>,
    ) -> Result<Vec<Item>, StoreError> {
        let items = self.items.lock().expect("items lock");
        Ok(items
            .values()
            .filter(|i| status.is_none_or(|s| i.status == s))
            .filter(|i| source.is_none_or(|s| i.source == s))
            .cloned()
            .collect())
    }

    fn record_decision(
        &self,
        id: &str,
        entry: DecisionLogEntry,
        new_status: ItemStatus,
    ) -> Result<Item, StoreError> {
        let mut items = self.items.lock().expect("items lock");
        let item = items
            .get_mut(id)
            .ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        item.decision_log.push(entry);
        item.status = new_status;
        Ok(item.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ProducerError;

    fn descriptor() -> ActionDescriptor {
        ActionDescriptor {
            target: "comms".into(),
            operation: "send_email".into(),
            params: serde_json::json!({ "to": "shiv@fastmail.com" }),
        }
    }

    #[test]
    fn create_assigns_monotonic_ids() {
        let store = InMemoryStore::new();
        let a = store
            .create("plane", "t", "ctx", "rec", &descriptor())
            .expect("create");
        let b = store
            .create("plane", "t", "ctx", "rec", &descriptor())
            .expect("create");
        assert!(b.id > a.id);
        assert_eq!(a.status, ItemStatus::Pending);
    }

    #[test]
    fn create_rejects_producer_bugs() {
        let store = InMemoryStore::new();
        let err = store
            .create("plane", "t", "", "rec", &descriptor())
            .unwrap_err();
        assert!(err.to_string().contains("no injected context"));
        let _ = ProducerError::MissingContext;
    }

    #[test]
    fn list_filters_by_status_and_source() {
        let store = InMemoryStore::new();
        store
            .create("plane", "t", "ctx", "rec", &descriptor())
            .expect("create");
        store
            .create("telegram", "t2", "ctx", "rec", &descriptor())
            .expect("create");
        assert_eq!(store.list(None, None).expect("list").len(), 2);
        assert_eq!(
            store
                .list(Some(ItemStatus::Pending), Some("telegram"))
                .expect("list")
                .len(),
            1
        );
    }

    #[test]
    fn decision_log_is_append_only_and_sets_status() {
        let store = InMemoryStore::new();
        let item = store
            .create("plane", "t", "ctx", "rec", &descriptor())
            .expect("create");
        let entry = DecisionLogEntry {
            at: "2026-11-17T00:00:00Z".into(),
            actor: "shiv".into(),
            decision: "approve".into(),
            note: None,
        };
        let updated = store
            .record_decision(&item.id, entry.clone(), ItemStatus::Dispatched)
            .expect("decide");
        assert_eq!(updated.status, ItemStatus::Dispatched);
        assert_eq!(updated.decision_log.len(), 1);
        assert!(store
            .record_decision("dkt-999", entry, ItemStatus::Dismissed)
            .is_err());
    }
}
