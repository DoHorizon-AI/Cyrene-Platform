//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  Strict JSON byte parser                                            │
//! │  Module: cy_workspace_product_contracts::strict_json                │
//! │  Role: Parse JSON without duplicate keys or lossy byte coercion.      │
//! │                                                                     │
//! │  模块职责：拒绝重复键与非法字节，供请求和响应 schema/scope 校验共用。 │
//! └─────────────────────────────────────────────────────────────────────┘

use std::collections::BTreeSet;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// Shared maximum size for a Product request or response JSON body.
pub const PRODUCT_JSON_BYTES_LIMIT: usize = 4 * 1024 * 1024;

const DEFAULT_MAX_JSON_BYTES: usize = PRODUCT_JSON_BYTES_LIMIT;
const DUPLICATE_KEY_MARKER: &str = "__CYRENE_STRICT_JSON_DUPLICATE_OBJECT_KEY__";

/// Stable parse failures for untrusted JSON byte strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StrictJsonError {
    /// The input exceeded the byte limit supplied by the server caller.
    #[error("STRICT_JSON_TOO_LARGE: maximum {max_bytes} bytes")]
    TooLarge { max_bytes: usize },
    /// The byte string is not valid UTF-8.
    #[error("STRICT_JSON_INVALID_UTF8")]
    InvalidUtf8,
    /// An object contains the same decoded key more than once.
    #[error("STRICT_JSON_DUPLICATE_OBJECT_KEY")]
    DuplicateKey,
    /// The input is not one complete, valid JSON document.
    #[error("STRICT_JSON_INVALID_DOCUMENT")]
    InvalidJson,
}

/// Parses Product JSON under the shared 4 MiB body limit.
///
/// This helper validates a borrowed byte slice and never rewrites it. Callers
/// keep the original bytes for forwarding after schema and scope checks.
pub fn parse_json_bytes(bytes: &[u8]) -> Result<Value, StrictJsonError> {
    parse_json_bytes_with_limit(bytes, DEFAULT_MAX_JSON_BYTES)
}

/// Parses JSON under an explicit server-owned byte limit.
///
/// The limit is supplied by trusted boundary code so the HTTP invocation
/// envelope can use its 6 MiB cap while Product bodies use 4 MiB. Invalid
/// UTF-8, malformed/trailing JSON, and duplicate keys are reported separately.
pub fn parse_json_bytes_with_limit(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<Value, StrictJsonError> {
    if bytes.len() > max_bytes {
        return Err(StrictJsonError::TooLarge { max_bytes });
    }
    let source = std::str::from_utf8(bytes).map_err(|_| StrictJsonError::InvalidUtf8)?;
    let mut deserializer = serde_json::Deserializer::from_str(source);
    let StrictValue(value) = StrictValue::deserialize(&mut deserializer).map_err(classify_error)?;
    deserializer.end().map_err(classify_error)?;
    Ok(value)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StrictValueVisitor;

        impl<'de> Visitor<'de> for StrictValueVisitor {
            type Value = StrictValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictValue(Value::Bool(value)))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictValue(Value::Number(Number::from(value))))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictValue(Value::Number(Number::from(value))))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let number = Number::from_f64(value)
                    .ok_or_else(|| E::custom("JSON number must be finite"))?;
                Ok(StrictValue(Value::Number(number)))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictValue(Value::String(value.to_owned())))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictValue(Value::String(value)))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(StrictValue(Value::Null))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(StrictValue(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(StrictValue(Value::Array(values)))
            }

            fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut keys = BTreeSet::new();
                let mut values = Map::new();
                while let Some(key) = object.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom(DUPLICATE_KEY_MARKER));
                    }
                    let StrictValue(value) = object.next_value()?;
                    values.insert(key, value);
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }

        deserializer.deserialize_any(StrictValueVisitor)
    }
}

fn classify_error(error: serde_json::Error) -> StrictJsonError {
    if error.to_string().starts_with(DUPLICATE_KEY_MARKER) {
        StrictJsonError::DuplicateKey
    } else {
        StrictJsonError::InvalidJson
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse_json_bytes, parse_json_bytes_with_limit, StrictJsonError, PRODUCT_JSON_BYTES_LIMIT,
    };
    use serde_json::json;

    #[test]
    fn parses_nested_values_and_detects_duplicates_recursively() {
        let bytes = br#"{"items":[{"id":"one"}],"enabled":true}"#;
        let original = bytes.to_vec();
        assert_eq!(
            parse_json_bytes(bytes).unwrap(),
            json!({"items":[{"id":"one"}],"enabled":true})
        );
        assert_eq!(&bytes[..], original.as_slice());

        assert_eq!(
            parse_json_bytes(br#"{"items":[{"workspaceId":"w1","workspaceId":"w2"}]}"#),
            Err(StrictJsonError::DuplicateKey)
        );
    }

    #[test]
    fn compares_decoded_object_keys_and_rejects_trailing_documents() {
        assert_eq!(
            parse_json_bytes(br#"{"a":1,"\u0061":2}"#),
            Err(StrictJsonError::DuplicateKey)
        );
        assert_eq!(
            parse_json_bytes(b"{} {}"),
            Err(StrictJsonError::InvalidJson)
        );
    }

    #[test]
    fn reports_utf8_syntax_and_byte_limit_failures_explicitly() {
        assert_eq!(parse_json_bytes(&[0xff]), Err(StrictJsonError::InvalidUtf8));
        assert_eq!(
            parse_json_bytes(b"{not-json}"),
            Err(StrictJsonError::InvalidJson)
        );
        let oversized = vec![b' '; PRODUCT_JSON_BYTES_LIMIT + 1];
        assert_eq!(
            parse_json_bytes(&oversized),
            Err(StrictJsonError::TooLarge {
                max_bytes: PRODUCT_JSON_BYTES_LIMIT
            })
        );
        assert_eq!(
            parse_json_bytes_with_limit(b"null", 3),
            Err(StrictJsonError::TooLarge { max_bytes: 3 })
        );
    }
}
