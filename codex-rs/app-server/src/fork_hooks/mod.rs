//! FORK: everything the fork wires into `codex-app-server`.
//!
//! Upstream files only call the hook functions below, one `// FORK:` line each, so pulling
//! upstream rarely conflicts. Host binaries (for example hcodex) plug in through the public
//! `add_*` functions instead of this crate depending on them.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::Weak;

use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_extension_api::ExtensionRegistryBuilder;

/// Installs host extensions into each thread extension registry the app-server builds.
pub type ExtensionInstaller =
    dyn Fn(&mut ExtensionRegistryBuilder<Config>, Weak<ThreadManager>) + Send + Sync;

static EXTENSION_INSTALLERS: RwLock<Vec<Arc<ExtensionInstaller>>> = RwLock::new(Vec::new());

/// Lets a host binary add thread extensions. Call at startup, before the app-server runs; every
/// registry built afterwards runs the installers in registration order.
pub fn add_extension_installer(
    installer: impl Fn(&mut ExtensionRegistryBuilder<Config>, Weak<ThreadManager>)
    + Send
    + Sync
    + 'static,
) {
    EXTENSION_INSTALLERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Arc::new(installer));
}

/// Extensions: runs the installers hosts registered with [`add_extension_installer`].
pub(crate) fn install_extensions(
    builder: &mut ExtensionRegistryBuilder<Config>,
    thread_manager: Weak<ThreadManager>,
) {
    let installers = EXTENSION_INSTALLERS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    for installer in installers {
        installer(builder, thread_manager.clone());
    }
}

/// Resume: thread stores other than the local one have no rollout file. Turns a missing path into
/// an empty one so resume continues; [`drop_empty_path`] removes it from the response.
pub(crate) fn resume_rollout_path(rollout_path: Option<PathBuf>) -> Option<PathBuf> {
    Some(rollout_path.unwrap_or_default())
}

/// Resume response: an empty path (see [`resume_rollout_path`]) means there is no rollout file.
pub(crate) fn drop_empty_path(path: &mut Option<PathBuf>) {
    if path.as_ref().is_some_and(|path| path.as_os_str().is_empty()) {
        *path = None;
    }
}
