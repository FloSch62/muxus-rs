//! The zod validation the TypeScript runs on every untrusted body, ported.
//!
//! A schema becomes a Rust type that deserializes with serde (unknown keys
//! are dropped, like zod's default `strip`; defaults fill in like `.default()`)
//! plus an implementation of [`Validate`], which normalizes the value the way
//! zod's output does (`.trim()`) and then checks every limit. [`parse`] runs
//! both. Messages follow zod 4's wording, since routes hand the first issue's
//! message to the user.

use serde::de::DeserializeOwned;

use crate::js;

/// One failed check: where (`profile.target`) and why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ValidationError {
    /// Dotted path of the offending field; empty for the value itself.
    pub path: String,
    pub message: String,
}

impl ValidationError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }

    /// zod's message for a failed `.refine()` without one of its own.
    pub fn custom(path: impl Into<String>) -> Self {
        Self::new(path, "Invalid input")
    }

    /// The same error reported under a parent field.
    pub fn within(mut self, parent: &str) -> Self {
        self.path = join(parent, &self.path);
        self
    }
}

/// Join a parent path and a child key (`profile` + `target`).
pub fn join(parent: &str, child: &str) -> String {
    match (parent.is_empty(), child.is_empty()) {
        (true, _) => child.to_owned(),
        (_, true) => parent.to_owned(),
        _ => format!("{parent}.{child}"),
    }
}

/// A schema's checks and normalization, run after deserialization.
pub trait Validate {
    /// Normalize the way zod's output does (trimmed strings) and check every
    /// limit. `path` is where this value sits in the enclosing body.
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError>;

    /// [`Validate::validate_at`] for a top-level value.
    fn validate(&mut self) -> Result<(), ValidationError> {
        self.validate_at("")
    }
}

impl<T: Validate> Validate for Option<T> {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        match self {
            Some(value) => value.validate_at(path),
            None => Ok(()),
        }
    }
}

impl<T: Validate> Validate for Vec<T> {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        for (index, item) in self.iter_mut().enumerate() {
            item.validate_at(&join(path, &index.to_string()))?;
        }
        Ok(())
    }
}

/// Deserialize and validate one JSON value, like `schema.parse(value)`.
pub fn parse_value<T: DeserializeOwned + Validate>(
    value: serde_json::Value,
) -> Result<T, ValidationError> {
    let mut parsed: T = serde_json::from_value(value)
        .map_err(|error| ValidationError::new("", serde_message(&error)))?;
    parsed.validate()?;
    Ok(parsed)
}

/// Deserialize and validate JSON text, like `schema.parse(JSON.parse(text))`.
pub fn parse<T: DeserializeOwned + Validate>(text: &str) -> Result<T, ValidationError> {
    let mut parsed: T = serde_json::from_str(text)
        .map_err(|error| ValidationError::new("", serde_message(&error)))?;
    parsed.validate()?;
    Ok(parsed)
}

fn serde_message(error: &serde_json::Error) -> String {
    // serde's messages name Rust types; keep zod's prefix so callers can show
    // them the same way.
    format!("Invalid input: {error}")
}

/// `z.string().min(min).max(max)` on UTF-16 length.
pub fn string(path: &str, value: &str, min: usize, max: usize) -> Result<(), ValidationError> {
    let length = js::utf16_len(value);
    if length < min {
        return Err(ValidationError::new(
            path,
            format!("Too small: expected string to have >={min} characters"),
        ));
    }
    if length > max {
        return Err(ValidationError::new(
            path,
            format!("Too big: expected string to have <={max} characters"),
        ));
    }
    Ok(())
}

/// [`string`] for an optional field.
pub fn opt_string(
    path: &str,
    value: &Option<String>,
    min: usize,
    max: usize,
) -> Result<(), ValidationError> {
    match value {
        Some(value) => string(path, value, min, max),
        None => Ok(()),
    }
}

/// `z.string().trim().min(min).max(max)`: trims in place, then checks.
pub fn trimmed(
    path: &str,
    value: &mut String,
    min: usize,
    max: usize,
) -> Result<(), ValidationError> {
    js::trim_in_place(value);
    string(path, value, min, max)
}

/// [`trimmed`] for an optional field.
pub fn opt_trimmed(
    path: &str,
    value: &mut Option<String>,
    min: usize,
    max: usize,
) -> Result<(), ValidationError> {
    match value {
        Some(value) => trimmed(path, value, min, max),
        None => Ok(()),
    }
}

/// `z.number().min(min).max(max)` for any integer type.
pub fn range<T: PartialOrd + std::fmt::Display + Copy>(
    path: &str,
    value: T,
    min: T,
    max: T,
) -> Result<(), ValidationError> {
    if value < min {
        return Err(ValidationError::new(
            path,
            format!("Too small: expected number to be >={min}"),
        ));
    }
    if value > max {
        return Err(ValidationError::new(
            path,
            format!("Too big: expected number to be <={max}"),
        ));
    }
    Ok(())
}

/// [`range`] for an optional field.
pub fn opt_range<T: PartialOrd + std::fmt::Display + Copy>(
    path: &str,
    value: Option<T>,
    min: T,
    max: T,
) -> Result<(), ValidationError> {
    match value {
        Some(value) => range(path, value, min, max),
        None => Ok(()),
    }
}

/// `z.number().int().positive()`.
pub fn positive(path: &str, value: u32) -> Result<(), ValidationError> {
    if value == 0 {
        return Err(ValidationError::new(
            path,
            "Too small: expected number to be >0",
        ));
    }
    Ok(())
}

/// `z.number().int()` on a JavaScript number held as `f64`.
pub fn integer(path: &str, value: f64) -> Result<(), ValidationError> {
    if !value.is_finite() || value.fract() != 0.0 {
        return Err(ValidationError::new(
            path,
            "Invalid input: expected int, received number",
        ));
    }
    Ok(())
}

/// `z.array(...).min(min).max(max)`.
pub fn items(path: &str, len: usize, min: usize, max: usize) -> Result<(), ValidationError> {
    if len < min {
        return Err(ValidationError::new(
            path,
            format!("Too small: expected array to have >={min} items"),
        ));
    }
    if len > max {
        return Err(ValidationError::new(
            path,
            format!("Too big: expected array to have <={max} items"),
        ));
    }
    Ok(())
}

/// [`items`] for an optional array.
pub fn opt_items<T>(
    path: &str,
    value: &Option<Vec<T>>,
    min: usize,
    max: usize,
) -> Result<(), ValidationError> {
    match value {
        Some(value) => items(path, value.len(), min, max),
        None => Ok(()),
    }
}

/// Each string of an array checked with [`string`].
pub fn each_string(
    path: &str,
    values: &[String],
    min: usize,
    max: usize,
) -> Result<(), ValidationError> {
    for (index, value) in values.iter().enumerate() {
        string(&join(path, &index.to_string()), value, min, max)?;
    }
    Ok(())
}

/// Each string of an array trimmed and checked with [`trimmed`].
pub fn each_trimmed(
    path: &str,
    values: &mut [String],
    min: usize,
    max: usize,
) -> Result<(), ValidationError> {
    for (index, value) in values.iter_mut().enumerate() {
        trimmed(&join(path, &index.to_string()), value, min, max)?;
    }
    Ok(())
}

/// `z.string().regex(pattern)`; `source` is the pattern as zod prints it.
pub fn pattern(
    path: &str,
    value: &str,
    matches: bool,
    source: &str,
) -> Result<(), ValidationError> {
    if !matches {
        return Err(ValidationError::new(
            path,
            format!("Invalid string: must match pattern {source}"),
        ));
    }
    let _ = value;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_zod_messages() {
        assert_eq!(
            string("", "", 1, 5).unwrap_err().message,
            "Too small: expected string to have >=1 characters"
        );
        assert_eq!(
            range("port", 0u32, 1, 65535).unwrap_err().message,
            "Too small: expected number to be >=1"
        );
        assert_eq!(
            items("", 2, 0, 1).unwrap_err().message,
            "Too big: expected array to have <=1 items"
        );
        let mut value = "  ".to_owned();
        assert!(trimmed("title", &mut value, 1, 500).is_err());
        assert_eq!(value, "");
    }
}
