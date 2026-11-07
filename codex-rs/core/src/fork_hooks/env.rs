//! Per-turn environment variables for command hooks and exec tools.
//!
//! Clients pass variables through the existing `turn/start` field
//! `responsesapi_client_metadata` using keys prefixed with [`FORK_ENV_METADATA_PREFIX`]
//! (for example `fork.env.TRACE_ID`). The prefix is stripped and the remainder becomes the
//! variable name. These entries are removed from metadata forwarded to model providers and MCP
//! servers.
//!
//! A recovered turn has no client metadata, so hosts can seed [`ForkTurnEnv`] through the
//! turn's extension data instead; metadata entries win over seeded values.

use std::collections::BTreeMap;

use crate::codex_thread::CodexThread;
use crate::session::turn_context::TurnContext;

/// Metadata key prefix that marks an entry as a child-process environment variable.
pub const FORK_ENV_METADATA_PREFIX: &str = "fork.env.";

/// Environment variables seeded by a host for one turn, for example when recovering it.
///
/// Turn extension data is inherited by later turns, so the value applies only to `turn_id`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForkTurnEnv {
    pub turn_id: String,
    pub env: BTreeMap<String, String>,
}

/// Returns whether `key` is a fork env entry that must not leave the process.
pub(crate) fn is_fork_env_metadata_key(key: &str) -> bool {
    key.starts_with(FORK_ENV_METADATA_PREFIX)
}

/// Resolves the environment variables that child processes of `turn` should receive.
pub(crate) fn turn_env(turn: &TurnContext) -> BTreeMap<String, String> {
    let mut env = turn
        .extension_data
        .get::<ForkTurnEnv>()
        .filter(|seeded| seeded.turn_id == turn.sub_id)
        .map(|seeded| seeded.env.clone())
        .unwrap_or_default();
    env.extend(env_from_metadata(
        turn.turn_metadata_state
            .responsesapi_client_metadata_snapshot(),
    ));
    env
}

/// [`turn_env`] as the map type used by command launchers.
pub(crate) fn turn_env_map(turn: &TurnContext) -> std::collections::HashMap<String, String> {
    turn_env(turn).into_iter().collect()
}

fn env_from_metadata(metadata: BTreeMap<String, String>) -> BTreeMap<String, String> {
    metadata
        .into_iter()
        .filter_map(|(key, value)| {
            let name = key.strip_prefix(FORK_ENV_METADATA_PREFIX)?;
            // Dropped rather than rejected so no upstream request handler needs a fork check.
            (is_valid_env_name(name) && !value.contains('\0')).then(|| (name.to_string(), value))
        })
        .collect()
}

/// Accepts portable shell variable names only, so values cannot smuggle `=` or NUL.
fn is_valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

impl CodexThread {
    /// FORK: returns the active regular turn's ID and resolved fork env, if a turn is running.
    pub async fn fork_active_turn_env(&self) -> Option<(String, BTreeMap<String, String>)> {
        let active = self.session.active_turn.lock().await;
        let task = active.as_ref()?.task.as_ref()?;
        Some((
            task.turn_context.sub_id.clone(),
            turn_env(&task.turn_context),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn metadata_prefix_maps_to_valid_env_entries_only() {
        let metadata = BTreeMap::from([
            ("fork.env.TRACE_ID".to_string(), "abc".to_string()),
            ("fork.env._PRIVATE".to_string(), "p".to_string()),
            ("fork.env.1BAD".to_string(), "x".to_string()),
            ("fork.env.BAD-NAME".to_string(), "x".to_string()),
            ("fork.env.".to_string(), "x".to_string()),
            ("fork.env.NUL".to_string(), "a\0b".to_string()),
            ("source".to_string(), "cli".to_string()),
        ]);

        assert_eq!(
            env_from_metadata(metadata),
            BTreeMap::from([
                ("TRACE_ID".to_string(), "abc".to_string()),
                ("_PRIVATE".to_string(), "p".to_string()),
            ])
        );
    }
}
