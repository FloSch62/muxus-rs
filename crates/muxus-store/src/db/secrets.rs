//! `assertSecretFree`: secrets are rejected at the persistence boundary.

use serde_json::Value;

use super::{DbError, DbResult};

/// Reject secrets at the persistence boundary. Profiles may contain key paths
/// and credential-reference IDs, but never passwords, passphrases, tokens, or
/// private-key material. `location` names the value in the error
/// (`config`, `workspace.layout`, …).
pub fn assert_secret_free(value: &Value, location: &str) -> DbResult<()> {
    match value {
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                assert_secret_free(item, &format!("{location}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (key, child) in map {
                let words = key_words(key);
                let sensitive = words.iter().any(|word| {
                    matches!(
                        word.as_str(),
                        "password" | "passphrase" | "secret" | "token" | "privatekey"
                    )
                }) || words
                    .windows(2)
                    .any(|pair| pair[0] == "private" && pair[1] == "key");
                let reference_only = words.last().is_some_and(|last| {
                    matches!(
                        last.as_str(),
                        "path" | "file" | "filename" | "ref" | "reference" | "id"
                    )
                });
                // This is an authentication policy flag, not password material.
                let safe_policy = key == "passwordOnly" && child.is_boolean();
                if sensitive && !reference_only && !safe_policy {
                    return Err(DbError::Secret(format!(
                        "{location}.{key} must not be stored in profile or workspace data; use the encrypted password vault"
                    )));
                }
                assert_secret_free(child, &format!("{location}.{key}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The lowercase words of a key: camelCase humps and every run of ASCII
/// letters and digits.
fn key_words(key: &str) -> Vec<String> {
    // key.replace(/([a-z0-9])([A-Z])/g, '$1 $2').split(/[^a-z0-9]+/i)
    let mut spaced = String::with_capacity(key.len() + 4);
    let mut previous: Option<char> = None;
    for c in key.chars() {
        if let Some(p) = previous
            && (p.is_ascii_lowercase() || p.is_ascii_digit())
            && c.is_ascii_uppercase()
        {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    spaced
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rejects_secrets_anywhere_in_persisted_json() {
        let check = |value: Value| assert_secret_free(&value, "config");
        let error = check(json!({ "nested": { "password": "hunter2" } }))
            .unwrap_err()
            .to_string();
        assert!(error.contains("password vault"), "{error}");
        assert_eq!(
            error,
            "config.nested.password must not be stored in profile or workspace data; use the encrypted password vault"
        );
        assert!(
            check(json!({ "auth": { "privateKeyPem": "-----BEGIN PRIVATE KEY-----" } })).is_err()
        );
        assert!(check(json!({ "auth": { "api_token_value": "secret" } })).is_err());
        assert!(
            check(json!({ "auth": {
                "privateKeyPath": "/home/alice/.ssh/id_ed25519",
                "passwordCredentialRefId": "credential-1",
            } }))
            .is_ok()
        );
        assert!(check(json!({ "passwordOnly": true })).is_ok());
        assert!(check(json!({ "passwordOnly": "yes" })).is_err());
        assert!(
            check(json!([{ "secret": 1 }]))
                .unwrap_err()
                .to_string()
                .starts_with("config[0].secret")
        );
    }

    #[test]
    fn splits_keys_like_the_typescript() {
        assert_eq!(key_words("privateKeyPem"), ["private", "key", "pem"]);
        assert_eq!(key_words("api_token_value"), ["api", "token", "value"]);
        assert_eq!(key_words("aBC"), ["a", "bc"]);
    }
}
