//! FORK: request-time checks for `fork.env.*` Responses client metadata.
//!
//! Core strips the prefix and exports the entries to child processes; without these checks an
//! invalid variable name is silently dropped and oversized values are accepted.

use std::collections::HashMap;

use codex_app_server_protocol::JSONRPCErrorError;

use crate::error_code::invalid_request;

/// Validates the `fork.env.*` entries of one metadata map; other keys are not checked.
pub(crate) fn validate(
    metadata: Option<&HashMap<String, String>>,
) -> Result<(), JSONRPCErrorError> {
    let Some(metadata) = metadata else {
        return Ok(());
    };
    codex_core::validate_fork_env_metadata(metadata).map_err(|message| {
        invalid_request(format!("invalid responsesapiClientMetadata: {message}"))
    })
}

/// Validates both the replacement and the patch metadata of a turn request.
pub(crate) fn validate_turn(
    metadata: Option<&HashMap<String, String>>,
    patch: Option<&HashMap<String, String>>,
) -> Result<(), JSONRPCErrorError> {
    validate(metadata)?;
    validate(patch)
}
