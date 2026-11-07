//! FORK: per-turn environment (`fork.env.*`) for command hooks. Child module of
//! `engine::command_runner` (declared at the end of that file) so it can reach the runtime's
//! private environment snapshot.

use std::collections::HashMap;
#[cfg(not(windows))]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::sync::Arc;

use super::CommandHookRuntime;

impl CommandHookRuntime {
    /// Returns a runtime whose command environment includes `overrides`.
    pub(crate) fn with_command_env(&self, overrides: &HashMap<String, String>) -> Self {
        if overrides.is_empty() {
            return self.clone();
        }
        let mut environment = self.environment.as_ref().clone();
        environment.retain(|(key, _)| {
            !overrides.keys().any(|name| {
                #[cfg(windows)]
                {
                    key.to_str()
                        .is_some_and(|key| key.eq_ignore_ascii_case(name))
                }
                #[cfg(not(windows))]
                {
                    key == OsStr::new(name)
                }
            })
        });
        environment.extend(
            overrides
                .iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value))),
        );
        let mut runtime = self.clone();
        runtime.environment = Arc::new(environment);
        runtime
    }
}
