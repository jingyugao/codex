//! FORK: everything the fork wires into `codex-app-server`.
//!
//! Upstream files only call the hook functions below, one `// FORK:` line each, so pulling
//! upstream rarely conflicts. Fork features themselves live in `codex-rs/fork/`.

use std::path::PathBuf;
use std::sync::Weak;

use codex_core::ThreadManager;
use codex_extension_api::ExtensionRegistryBuilder;

/// Extensions: cross-pod turn handoff and sandbox binding; inert unless their env vars are set.
pub(crate) fn install_extensions<C: Sync + 'static>(
    builder: &mut ExtensionRegistryBuilder<C>,
    thread_manager: Weak<ThreadManager>,
) {
    codex_fork_ext::install(builder, thread_manager);
}

/// Resume: thread stores other than the local one have no rollout file. Turns a missing path into
/// an empty one so resume continues; [`drop_empty_path`] removes it from the response.
pub(crate) fn resume_rollout_path(rollout_path: Option<PathBuf>) -> Option<PathBuf> {
    Some(rollout_path.unwrap_or_default())
}

/// Resume response: an empty path (see [`resume_rollout_path`]) means there is no rollout file.
pub(crate) fn drop_empty_path(path: &mut Option<PathBuf>) {
    if path
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        *path = None;
    }
}
