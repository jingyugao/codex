//! Queued items keep their Responses client metadata inside the stored `TurnInput` JSON, as an
//! extra field of the `UserInput` variant. Upstream deserialization ignores unknown fields, so
//! payloads stay readable by unmodified queue code; items without metadata are stored exactly
//! as upstream stores them.

use std::collections::HashMap;

use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_protocol::ThreadId;
use codex_thread_store::MAX_QUEUE_ITEMS;
use codex_thread_store::QueueStore;
use codex_thread_store::ThreadStoreError;
use serde_json::Value;

const VARIANT: &str = "UserInput";
const METADATA_FIELD: &str = "fork_responsesapi_client_metadata";

/// Responses client metadata stored with one queued item.
pub type QueuedMetadata = Option<HashMap<String, String>>;

/// Serializes `input` for the queue store, embedding `metadata` when present.
pub fn encode_payload(
    input: &TurnInput,
    metadata: &QueuedMetadata,
) -> Result<String, serde_json::Error> {
    let Some(metadata) = metadata.as_ref().filter(|metadata| !metadata.is_empty()) else {
        return serde_json::to_string(input);
    };
    let mut value = serde_json::to_value(input)?;
    if let Some(fields) = value.get_mut(VARIANT).and_then(Value::as_object_mut) {
        fields.insert(METADATA_FIELD.to_string(), serde_json::to_value(metadata)?);
    }
    serde_json::to_string(&value)
}

/// Metadata embedded in a stored payload; `None` when absent or unreadable.
pub fn payload_metadata(payload: &str) -> QueuedMetadata {
    let mut value: Value = serde_json::from_str(payload).ok()?;
    let metadata = value.get_mut(VARIANT)?.get_mut(METADATA_FIELD)?.take();
    serde_json::from_value(metadata).ok()
}

/// Metadata of the queued item `queued_item_id`; `None` when it has none or no longer exists.
pub async fn stored_metadata(
    queue: &dyn QueueStore,
    thread_id: ThreadId,
    queued_item_id: &str,
) -> Result<QueuedMetadata, ThreadStoreError> {
    Ok(queue
        .list_page(thread_id, /*offset*/ 0, MAX_QUEUE_ITEMS)
        .await?
        .into_iter()
        .find(|record| record.id == queued_item_id)
        .and_then(|record| payload_metadata(&record.payload)))
}

/// Turn request for a dequeued item, carrying the metadata it was queued with.
pub fn turn_request(input: TurnInput, metadata: QueuedMetadata) -> TurnInputRequest {
    TurnInputRequest::new(input).with_responses_metadata(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::user_input::UserInput;
    use pretty_assertions::assert_eq;

    fn input() -> TurnInput {
        TurnInput::UserInput {
            content: vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }],
            client_id: Some("user-1:abc".to_string()),
        }
    }

    #[test]
    fn without_metadata_keeps_upstream_format() {
        let plain = serde_json::to_string(&input()).expect("plain");
        assert_eq!(encode_payload(&input(), &None).expect("encode"), plain);
        assert_eq!(
            encode_payload(&input(), &Some(HashMap::new())).expect("encode"),
            plain
        );
        assert_eq!(payload_metadata(&plain), None);
    }

    #[test]
    fn metadata_round_trips_and_upstream_parsing_still_works() {
        let metadata = Some(HashMap::from([(
            "fork.env.COLONY_USER".to_string(),
            "u1".to_string(),
        )]));
        let payload = encode_payload(&input(), &metadata).expect("encode");
        assert_eq!(payload_metadata(&payload), metadata);
        assert_eq!(
            serde_json::from_str::<TurnInput>(&payload).expect("upstream parse"),
            input()
        );
    }
}
