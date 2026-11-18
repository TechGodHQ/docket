//! docket-core: the decision-ledger domain model.
//!
//! Docket is a decision LEDGER, never an executor. Core knows items,
//! decisions, and declarative action descriptors — it never performs an
//! action itself, and it contains no LLM. See `.creed/config/context.md`
//! for the design laws.

pub mod config;
pub mod store;
pub mod types;

pub use config::DocketConfig;
pub use store::{InMemoryStore, ItemStore, StoreError};
pub use types::{ActionDescriptor, DecisionLogEntry, DocketInfo, Item, ItemStatus, ProducerError};
