//! FORK: child module of `turn_metadata` (declared at the end of that file) so it can read the
//! private client metadata.

use std::collections::BTreeMap;

use super::TurnMetadataState;

impl TurnMetadataState {
    /// Snapshot of the client metadata used to derive per-turn child-process env.
    pub(crate) fn responsesapi_client_metadata_snapshot(&self) -> BTreeMap<String, String> {
        self.responsesapi_client_metadata
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
