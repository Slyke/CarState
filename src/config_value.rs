//! Reject JSON5 non-finite numbers before serde_json can normalize them to null.
use serde::{
    de::{self, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;
use std::fmt;
pub struct FiniteValue(pub Value);
impl<'de> Deserialize<'de> for FiniteValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct FiniteVisitor;
        impl<'de> Visitor<'de> for FiniteVisitor {
            type Value = FiniteValue;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON5 with finite numbers")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(FiniteValue(Value::Null))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(FiniteValue(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(FiniteValue(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(FiniteValue(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| FiniteValue(n.into()))
                    .ok_or_else(|| E::custom("non-finite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(FiniteValue(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(FiniteValue(v.into()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(FiniteValue(v)) = a.next_element()? {
                    values.push(v);
                }
                Ok(FiniteValue(values.into()))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut object = serde_json::Map::new();
                while let Some((k, FiniteValue(v))) = a.next_entry::<String, FiniteValue>()? {
                    object.insert(k, v);
                }
                Ok(FiniteValue(object.into()))
            }
        }
        d.deserialize_any(FiniteVisitor)
    }
}
