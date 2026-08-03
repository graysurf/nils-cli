use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

pub(crate) fn from_slice(input: &[u8]) -> serde_json::Result<Value> {
    serde_json::from_slice::<StrictValue>(input).map(|value| value.0)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("strict JSON without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(|value| StrictValue(Value::Number(value)))
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_string())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = Map::new();
        while let Some(key) = entries.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate object key: {key}"
                )));
            }
            let value = entries.next_value::<StrictValue>()?;
            object.insert(key, value.0);
        }
        Ok(StrictValue(Value::Object(object)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn parse(input: &str) -> serde_json::Result<Value> {
        from_slice(input.as_bytes())
    }

    #[test]
    fn every_scalar_kind_round_trips() {
        assert_eq!(parse("true").unwrap(), json!(true));
        assert_eq!(parse("false").unwrap(), json!(false));
        assert_eq!(parse("null").unwrap(), Value::Null);
        assert_eq!(parse("\"text\"").unwrap(), json!("text"));
        assert_eq!(parse("-7").unwrap(), json!(-7));
        assert_eq!(parse("7").unwrap(), json!(7));
        assert_eq!(parse("1.5").unwrap(), json!(1.5));
        // A u64 above i64::MAX must not be narrowed.
        assert_eq!(
            parse("18446744073709551615").unwrap(),
            json!(18_446_744_073_709_551_615_u64)
        );
    }

    #[test]
    fn nested_containers_are_reconstructed_in_order() {
        let value = parse(r#"{"a":[1,{"b":"c"},[]],"d":{}}"#).unwrap();

        assert_eq!(value, json!({"a": [1, {"b": "c"}, []], "d": {}}));
        assert_eq!(parse("[]").unwrap(), json!([]));
        assert_eq!(parse("{}").unwrap(), json!({}));
    }

    #[test]
    fn a_duplicate_object_key_is_rejected_instead_of_last_write_wins() {
        // serde_json's own parser silently keeps the last value; a hook payload
        // must not be able to smuggle a second value past an earlier check.
        let err = parse(r#"{"decision":"allow","decision":"deny"}"#)
            .expect_err("duplicate key must be rejected");

        assert!(
            err.to_string().contains("duplicate object key: decision"),
            "{err}"
        );
    }

    #[test]
    fn a_duplicate_key_nested_inside_a_container_is_still_rejected() {
        let err = parse(r#"{"outer":[{"k":1,"k":2}]}"#).expect_err("nested duplicate");
        assert!(err.to_string().contains("duplicate object key: k"), "{err}");

        // Distinct keys at the same depth stay valid.
        assert_eq!(
            parse(r#"{"outer":[{"k":1},{"k":2}]}"#).unwrap(),
            json!({"outer": [{"k": 1}, {"k": 2}]}),
        );
    }

    #[test]
    fn malformed_input_still_surfaces_a_parse_error() {
        assert!(parse("{").is_err());
        assert!(parse("").is_err());
        assert!(from_slice(&[0xff, 0xfe]).is_err());
    }
}
