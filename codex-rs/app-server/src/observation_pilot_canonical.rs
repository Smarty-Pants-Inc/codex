//! Version2 decision encoding matches the accepted Python helper's exact recipe.
//! Not used for version1 decisions or the prepared credential envelope.

use serde_json::Value;
use std::io;

/// Schema numbers are nonnegative JavaScript-safe integers, not arbitrary JSON
/// floats. Explicit key sorting avoids depending on serde_json's map backend.
pub(super) fn encode(value: &Value) -> io::Result<Vec<u8>> {
    fn append(value: &Value, output: &mut Vec<u8>) -> io::Result<()> {
        match value {
            Value::Object(fields) => {
                output.push(b'{');
                let mut entries: Vec<_> = fields.iter().collect();
                // UTF8 lexicographic order equals Unicode scalar order for valid
                // Rust strings; unlike JS's default UTF16 sort, this matches Python.
                entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
                for (index, (key, value)) in entries.into_iter().enumerate() {
                    if index != 0 {
                        output.push(b',');
                    }
                    output.extend(serde_json::to_vec(key)?);
                    output.push(b':');
                    append(value, output)?;
                }
                output.push(b'}');
            }
            Value::Array(values) => {
                output.push(b'[');
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        output.push(b',');
                    }
                    append(value, output)?;
                }
                output.push(b']');
            }
            Value::Number(number) => {
                let integer = number
                    .as_u64()
                    .filter(|number| *number <= 9_007_199_254_740_991)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "pilot integer required")
                    })?;
                output.extend(integer.to_string().bytes());
            }
            Value::String(_) | Value::Bool(_) | Value::Null => {
                output.extend(serde_json::to_vec(value)?);
            }
        }
        Ok(())
    }
    let mut bytes = Vec::new();
    append(value, &mut bytes)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(super) fn validate(bytes: &[u8]) -> io::Result<()> {
    let value: Value = serde_json::from_slice(bytes)?;
    // Duplicate keys, alternate escapes/numeric spelling and whitespace cannot
    // survive this exact-byte comparison, even with their own matching digest.
    if encode(&value)? != bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "noncanonical pilot decision",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "observation_pilot_canonical_tests.rs"]
mod tests;
