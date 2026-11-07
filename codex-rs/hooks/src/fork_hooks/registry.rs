//! FORK: per-turn environment (`fork.env.*`) for command hooks. Child module of `registry`
//! (declared at the end of that file) so it can reach the private hooks engine.

use std::collections::HashMap;

use super::Hooks;

impl Hooks {
    /// Returns hooks whose command runtime adds `overrides` to the session env snapshot.
    pub fn with_command_env(&self, overrides: &HashMap<String, String>) -> Self {
        let mut hooks = self.clone();
        hooks.engine.command_runtime = self.engine.command_runtime.with_command_env(overrides);
        hooks
    }
}
