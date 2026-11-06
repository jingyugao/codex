//! Fork-only support for per-turn env (`fork.env.*` Responses client metadata):
//!
//! - [`validate_fork_env_metadata`]: request-time checks used by `turn/start`, `turn/steer` and
//!   `thread/queue/add`.
//! - Queued-message metadata: `thread/queue/add` stores the metadata with the queued item and the
//!   queue applies it to the turn that later starts the item.
//!
//! Upstream files only call into this crate, so upstream rebases touch a few call sites.

mod metadata;
mod queued;

pub use metadata::MAX_FORK_ENV_ENTRIES;
pub use metadata::MAX_FORK_ENV_KEY_BYTES;
pub use metadata::MAX_FORK_ENV_VALUE_BYTES;
pub use metadata::validate_fork_env_metadata;
pub use metadata::validate_turn_metadata;
pub use queued::QueuedMetadata;
pub use queued::encode_payload;
pub use queued::payload_metadata;
pub use queued::stored_metadata;
pub use queued::turn_request;
