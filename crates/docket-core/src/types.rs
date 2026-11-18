//! Core item model. One `Item` = one decision waiting to happen.
//!
//! Design law (b): every item MUST ship with injected context and a
//! recommended action. An item missing either is a producer bug and is
//! rejected at the boundary by [`validate_item`] — it never enters the
//! ledger half-formed, because half-formed items cost Shiv attention,
//! and throughput-per-minute-of-Shiv's-attention is the product.

use serde::{Deserialize, Serialize};

/// A declarative instruction for an existing worker. Docket never
/// executes this itself (design law (a)): dispatch layers read it and
/// hand it to the named worker (e.g. `hermes-coding` for merges, `comms`
/// for email).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDescriptor {
    /// Target worker identity (e.g. "hermes-coding", "comms").
    pub target: String,
    /// Operation the target worker already knows how to perform
    /// (e.g. "merge_pr", "send_email").
    pub operation: String,
    /// Opaque parameters passed through to the target worker.
    pub params: serde_json::Value,
}

/// Ledger lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// Waiting on a decision.
    Pending,
    /// Decided; action dispatched to the target worker.
    Dispatched,
    /// Decided; no action to take.
    Dismissed,
}

impl ItemStatus {
    /// Stable wire string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Dispatched => "dispatched",
            Self::Dismissed => "dismissed",
        }
    }
}

/// One appended decision record. Append-only: history is never rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionLogEntry {
    /// RFC 3339 timestamp of the decision.
    pub at: String,
    /// Who decided (human or agent identity).
    pub actor: String,
    /// The decision (e.g. "approve", "dismiss", "defer").
    pub decision: String,
    /// Optional free-form rationale.
    #[serde(default)]
    pub note: Option<String>,
}

/// A decision-queue item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Ledger id (`dkt-…`), assigned by the store.
    pub id: String,
    /// Producing source (e.g. "plane", "telegram", "inbox").
    pub source: String,
    /// One-line summary.
    pub title: String,
    /// Injected context the decider needs — markdown (design law (b)).
    pub context: String,
    /// The recommended action, stated plainly (design law (b)).
    pub recommendation: String,
    /// Declarative dispatch instruction (design law (a)).
    pub action_descriptor: ActionDescriptor,
    /// Lifecycle status.
    pub status: ItemStatus,
    /// Append-only decision history.
    pub decision_log: Vec<DecisionLogEntry>,
}

/// Service identity payload for the `docket_info` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocketInfo {
    /// Always "docket".
    pub service: &'static str,
    /// Core version.
    pub version: &'static str,
    /// The invariant, stated.
    pub role: &'static str,
}

/// Rejection reasons for producer-boundary validation (design law (b)).
#[derive(Debug, thiserror::Error)]
pub enum ProducerError {
    /// Empty context — the producer failed to inject what the decider needs.
    #[error("producer bug: item has no injected context")]
    MissingContext,
    /// Empty recommendation — the producer failed to state the ask.
    #[error("producer bug: item has no recommended action")]
    MissingRecommendation,
    /// No action descriptor — docket would hold an undecidable item.
    #[error("producer bug: item has no action_descriptor")]
    MissingActionDescriptor,
    /// Empty target/operation pair on the descriptor.
    #[error("producer bug: action_descriptor has empty target or operation")]
    EmptyActionDescriptor,
}

/// Producer-boundary check (design law (b)). Every item entering the
/// ledger passes this; failures are producer bugs, not user errors.
///
/// # Errors
/// Returns the specific [`ProducerError`] for the first violated law.
pub fn validate_item(
    context: &str,
    recommendation: &str,
    action: Option<&ActionDescriptor>,
) -> Result<(), ProducerError> {
    if context.trim().is_empty() {
        return Err(ProducerError::MissingContext);
    }
    if recommendation.trim().is_empty() {
        return Err(ProducerError::MissingRecommendation);
    }
    let action = action.ok_or(ProducerError::MissingActionDescriptor)?;
    if action.target.trim().is_empty() || action.operation.trim().is_empty() {
        return Err(ProducerError::EmptyActionDescriptor);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ActionDescriptor {
        ActionDescriptor {
            target: "hermes-coding".into(),
            operation: "merge_pr".into(),
            params: serde_json::json!({ "pr": 1 }),
        }
    }

    #[test]
    fn valid_item_passes() {
        assert!(validate_item("ctx", "merge it", Some(&descriptor())).is_ok());
    }

    #[test]
    fn empty_context_is_producer_bug() {
        let err = validate_item("  ", "rec", Some(&descriptor())).unwrap_err();
        assert!(matches!(err, ProducerError::MissingContext));
    }

    #[test]
    fn empty_recommendation_is_producer_bug() {
        let err = validate_item("ctx", "", Some(&descriptor())).unwrap_err();
        assert!(matches!(err, ProducerError::MissingRecommendation));
    }

    #[test]
    fn missing_descriptor_is_producer_bug() {
        let err = validate_item("ctx", "rec", None).unwrap_err();
        assert!(matches!(err, ProducerError::MissingActionDescriptor));
    }

    #[test]
    fn empty_descriptor_target_is_producer_bug() {
        let bad = ActionDescriptor {
            target: "".into(),
            operation: "merge_pr".into(),
            params: serde_json::json!({}),
        };
        let err = validate_item("ctx", "rec", Some(&bad)).unwrap_err();
        assert!(matches!(err, ProducerError::EmptyActionDescriptor));
    }

    #[test]
    fn item_serde_roundtrip() {
        let item = Item {
            id: "dkt-1".into(),
            source: "plane".into(),
            title: "merge docket#1".into(),
            context: "CI green, reviewed".into(),
            recommendation: "merge".into(),
            action_descriptor: descriptor(),
            status: ItemStatus::Pending,
            decision_log: vec![],
        };
        let json = serde_json::to_string(&item).expect("serialize");
        let back: Item = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, item);
    }

    #[test]
    fn status_wire_strings() {
        assert_eq!(ItemStatus::Pending.as_str(), "pending");
        assert_eq!(ItemStatus::Dispatched.as_str(), "dispatched");
        assert_eq!(ItemStatus::Dismissed.as_str(), "dismissed");
    }
}
