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
use codex_protocol::ThreadId;
use codex_protocol::config_types::ShellEnvironmentPolicy;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_thread_store::ThreadStore;

pub use env::FORK_ENV_METADATA_PREFIX;
pub use env::ForkTurnEnv;

use crate::config::Config;
use crate::session::session::Session;
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
    THREAD_STORE_FACTORY
        .get()
        .and_then(|factory| factory(config))
}

/// A host-provided admission gate. Its permit lives until the turn start is committed.
pub type ThreadStartGate = dyn Fn(ThreadId) -> Option<Box<dyn Send>> + Send + Sync;

static THREAD_START_GATE: OnceLock<Box<ThreadStartGate>> = OnceLock::new();

/// Install the thread-aware admission gate once, before accepting requests.
pub fn set_thread_start_gate(
    gate: impl Fn(ThreadId) -> Option<Box<dyn Send>> + Send + Sync + 'static,
) -> bool {
    THREAD_START_GATE.set(Box::new(gate)).is_ok()
}

/// Preserve upstream admission and hold both permits through turn start.
pub(crate) fn admit_turn_start(
    extensions: &codex_extension_api::ExtensionRegistry<Config>,
    thread_id: ThreadId,
) -> Option<Box<dyn Send>> {
    let upstream = extensions.admit_turn_start()?;
    match THREAD_START_GATE.get() {
        Some(gate) => Some(Box::new((upstream, gate(thread_id)?))),
        None => Some(upstream),
    }
}

/// Protect all task registrations, including those without a turn-start admission request.
pub(crate) async fn admit_task_start(
    session: &Arc<Session>,
    turn_context: &TurnContext,
) -> Option<Box<dyn Send>> {
    let permit = match THREAD_START_GATE.get() {
        Some(gate) => gate(session.thread_id),
        None => Some(Box::new(()) as Box<dyn Send>),
    };
    if permit.is_none() {
        session
            .send_event(
                turn_context,
                EventMsg::Error(ErrorEvent {
                    message: "Workspace is moving; reconnect and retry".to_string(),
                    codex_error_info: Some(CodexErrorInfo::Other),
                    misalignment: None,
                    rejected_audio_file_ids: Vec::new(),
                }),
            )
            .await;
    }
    permit
}
