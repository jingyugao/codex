use std::collections::HashMap;

use codex_core::FORK_ENV_METADATA_PREFIX;

/// Most `fork.env.*` entries accepted in one metadata map.
pub const MAX_FORK_ENV_ENTRIES: usize = 16;
/// Longest accepted metadata key, prefix included.
pub const MAX_FORK_ENV_KEY_BYTES: usize = 64;
/// Longest accepted value.
pub const MAX_FORK_ENV_VALUE_BYTES: usize = 128;

/// Rejects `fork.env.*` entries that Core would otherwise drop silently or pass on unchecked.
///
/// Other metadata keys are not checked, so upstream behavior for them does not change.
pub fn validate_fork_env_metadata(
    metadata: Option<&HashMap<String, String>>,
) -> Result<(), String> {
    let mut count = 0;
    for (key, value) in metadata.into_iter().flatten() {
        let Some(name) = key.strip_prefix(FORK_ENV_METADATA_PREFIX) else {
            continue;
        };
        count += 1;
        if count > MAX_FORK_ENV_ENTRIES {
            return Err(format!(
                "at most {MAX_FORK_ENV_ENTRIES} {FORK_ENV_METADATA_PREFIX}* entries are allowed"
            ));
        }
        if key.len() > MAX_FORK_ENV_KEY_BYTES {
            return Err(format!(
                "metadata key `{key}` exceeds {MAX_FORK_ENV_KEY_BYTES} bytes"
            ));
        }
        if !is_valid_env_name(name) {
            return Err(format!(
                "metadata key `{key}` must name an environment variable matching [A-Za-z_][A-Za-z0-9_]*"
            ));
        }
        if value.len() > MAX_FORK_ENV_VALUE_BYTES {
            return Err(format!(
                "value of metadata key `{key}` exceeds {MAX_FORK_ENV_VALUE_BYTES} bytes"
            ));
        }
        if value.contains('\0') {
            return Err(format!("value of metadata key `{key}` contains NUL"));
        }
    }
    Ok(())
}

/// Validates the replacement and the patch metadata of a `turn/start` or `turn/steer` request.
pub fn validate_turn_metadata(
    metadata: Option<&HashMap<String, String>>,
    patch: Option<&HashMap<String, String>>,
) -> Result<(), String> {
    validate_fork_env_metadata(metadata)?;
    validate_fork_env_metadata(patch)
}

/// Same rule Core applies when exporting the variables.
fn is_valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn validate(entries: &[(&str, &str)]) -> Result<(), String> {
        let metadata: HashMap<String, String> = entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        validate_fork_env_metadata(Some(&metadata))
    }

    #[test]
    fn accepts_limits_and_ignores_other_keys() {
        let long_value = "v".repeat(MAX_FORK_ENV_VALUE_BYTES);
        let long_key = format!(
            "{FORK_ENV_METADATA_PREFIX}{}",
            "K".repeat(MAX_FORK_ENV_KEY_BYTES - FORK_ENV_METADATA_PREFIX.len())
        );
        assert_eq!(
            validate(&[
                ("fork.env.COLONY_USER", &long_value),
                (&long_key, "x"),
                ("not an env key", &"y".repeat(500)),
            ]),
            Ok(())
        );
        assert_eq!(validate_fork_env_metadata(None), Ok(()));
    }

    #[test]
    fn rejects_bad_entries() {
        let too_long_key = format!(
            "{FORK_ENV_METADATA_PREFIX}{}",
            "K".repeat(MAX_FORK_ENV_KEY_BYTES)
        );
        let too_long_value = "v".repeat(MAX_FORK_ENV_VALUE_BYTES + 1);
        for entries in [
            vec![("fork.env.BAD-NAME", "x")],
            vec![("fork.env.1BAD", "x")],
            vec![("fork.env.", "x")],
            vec![(too_long_key.as_str(), "x")],
            vec![("fork.env.OK", too_long_value.as_str())],
            vec![("fork.env.OK", "a\0b")],
        ] {
            assert!(validate(&entries).is_err(), "{entries:?}");
        }

        let names: Vec<String> = (0..=MAX_FORK_ENV_ENTRIES)
            .map(|index| format!("fork.env.V{index}"))
            .collect();
        let too_many: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "x")).collect();
        assert!(validate(&too_many).is_err());
    }
}
