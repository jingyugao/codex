//! FORK: everything the fork adds to `codex-core`.
//!
//! Upstream files only call the hook functions below, one `// FORK:` line each, so pulling
//! upstream rarely conflicts. Code that needs a private upstream field lives in this directory
//! too, as a child module of that upstream file (`#[path = "fork_hooks/…"] mod fork;`).

mod env;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

use codex_hooks::Hooks;
use codex_protocol::config_types::ShellEnvironmentPolicy;
use codex_thread_store::ThreadStore;

pub use env::FORK_ENV_METADATA_PREFIX;
pub use env::ForkTurnEnv;

use crate::config::Config;
use crate::session::turn_context::TurnContext;

/// Exec: the shell environment policy with the turn's fork env added to `set`, so the local,
/// remote and shell-snapshot paths all apply it. Codex-owned variables that exec injects after
/// applying the policy still take precedence.
pub(crate) fn exec_shell_policy(
    policy: &ShellEnvironmentPolicy,
    turn: &TurnContext,
) -> ShellEnvironmentPolicy {
    let mut policy = policy.clone();
    policy.r#set.extend(env::turn_env(turn));
    policy
}

/// Hooks: the session hooks with the turn's fork env added to command hooks run by this process.
pub(crate) fn hooks_with_turn_env(hooks: &Hooks, turn: &TurnContext) -> Hooks {
    hooks.with_command_env(&env::turn_env_map(turn))
}

/// Hooks: the env for a hook run in a remote environment. The turn's fork env goes first so
/// hook-specific values keep precedence.
pub(crate) fn environment_hook_env(
    turn: &TurnContext,
    env: HashMap<String, String>,
) -> HashMap<String, String> {
    let mut merged = env::turn_env_map(turn);
    merged.extend(env);
    merged
}

/// Metadata leaving the process (model provider, MCP servers): drop fork env entries, which are
/// for local child processes only.
pub(crate) fn strip_client_env(metadata: &mut BTreeMap<String, String>) {
    metadata.retain(|key, _| !env::is_fork_env_metadata_key(key));
}

/// Thread store: Postgres when `CODEX_FORK_THREAD_STORE` is set, else `None` for the configured
/// upstream store.
pub(crate) fn thread_store(config: &Config) -> Option<Arc<dyn ThreadStore>> {
    codex_fork_pg::thread_store_from_env(&config.model_provider_id)
}
