//! FORK: per-item Responses client metadata (and so `fork.env.*`) for queued messages.
//!
//! A queued message runs later as its own turn, so the metadata sent with
//! `thread/queue/add` is stored with the item and applied to the turn that starts it.
//! Items without metadata keep the upstream payload format: a bare `TurnInput`.
//! Items with metadata are wrapped in an envelope keyed by [`ENVELOPE_KEY`], which no
//! `TurnInput` variant uses, so the two formats never parse as each other.

use std::collections::HashMap;

use codex_core::TurnInput;
use codex_protocol::ThreadId;
use codex_thread_store::MAX_QUEUE_ITEMS;
use codex_thread_store::QueueStore;
use serde::Deserialize;
use serde::Serialize;

const ENVELOPE_KEY: &str = "forkQueuedInputV1";

/// Responses client metadata stored with one queued item.
pub type QueuedMetadata = Option<HashMap<String, String>>;

#[derive(Serialize, Deserialize)]
struct Envelope {
    #[serde(rename = "forkQueuedInputV1")]
    inner: EnvelopeBody,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvelopeBody {
    input: TurnInput,
    responsesapi_client_metadata: HashMap<String, String>,
}

/// Serializes `input` for the queue store, wrapping it only when metadata is present.
pub(crate) fn encode(
    input: &TurnInput,
    metadata: &QueuedMetadata,
) -> Result<String, serde_json::Error> {
    match metadata {
        Some(metadata) if !metadata.is_empty() => serde_json::to_string(&Envelope {
            inner: EnvelopeBody {
                input: input.clone(),
                responsesapi_client_metadata: metadata.clone(),
            },
        }),
        _ => serde_json::to_string(input),
    }
}

/// Metadata stored with `queued_item_id`, or `None` when it has none or is gone.
pub(crate) async fn queued_metadata(
    queue: &dyn QueueStore,
    thread_id: ThreadId,
    queued_item_id: &str,
) -> Result<QueuedMetadata, crate::QueueServiceError> {
    let Some(record) = queue
        .list_page(thread_id, /*offset*/ 0, MAX_QUEUE_ITEMS)
        .await?
        .into_iter()
        .find(|record| record.id == queued_item_id)
    else {
        return Ok(None);
    };
    Ok(decode(&record.payload)?.1)
}

/// Reads a stored payload in either format.
pub(crate) fn decode(payload: &str) -> Result<(TurnInput, QueuedMetadata), serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(payload)?;
    if value.get(ENVELOPE_KEY).is_some() {
        let Envelope { inner } = serde_json::from_value(value)?;
        let metadata = (!inner.responsesapi_client_metadata.is_empty())
            .then_some(inner.responsesapi_client_metadata);
        return Ok((inner.input, metadata));
    }
    Ok((serde_json::from_value(value)?, None))
}
