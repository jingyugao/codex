//! FORK: everything the fork adds to `codex-core`.
//!
//! Upstream files only call the hook functions below, one `// FORK:` line each, so pulling
//! upstream rarely conflicts. Host binaries (for example hcodex) plug in through the public
//! `set_*` functions instead of this crate depending on them. Code that needs a private upstream field lives in this directory
//! too, as a child module of that upstream file (`#[path = "fork_hooks/…"] mod fork;`).

mod env;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;

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

/// Builds a thread store for a config, or `None` to keep the configured upstream store.
pub type ThreadStoreFactory = dyn Fn(&Config) -> Option<Arc<dyn ThreadStore>> + Send + Sync;

static THREAD_STORE_FACTORY: OnceLock<Box<ThreadStoreFactory>> = OnceLock::new();

/// Lets a host binary replace the thread store (for example with one backed by a database).
/// Call once at startup, before any thread store is built. Returns `false` if a factory was
/// already set; the first one stays.
pub fn set_thread_store_factory(
    factory: impl Fn(&Config) -> Option<Arc<dyn ThreadStore>> + Send + Sync + 'static,
) -> bool {
    THREAD_STORE_FACTORY.set(Box::new(factory)).is_ok()
}

/// Thread store: the host's store when it registered a factory that returns one, else `None` for
/// the configured upstream store.
pub(crate) fn thread_store(config: &Config) -> Option<Arc<dyn ThreadStore>> {
    THREAD_STORE_FACTORY.get().and_then(|factory| factory(config))
}
