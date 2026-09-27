use super::{Error, Patch, Token, render};
use crate::config::TokenMapping;
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};
use serde_json::Value;
use std::{collections::BTreeMap, fmt};

struct Object(BTreeMap<String, Value>);
impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("one JSON object with unique properties")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Object, M::Error> {
                let mut fields = BTreeMap::new();
                while let Some((k, v)) = map.next_entry::<String, Value>()? {
                    if fields.insert(k, v).is_some() {
                        return Err(de::Error::custom("duplicate property"));
                    }
                }
                Ok(Object(fields))
            }
        }
        d.deserialize_map(ObjectVisitor)
    }
}
pub(super) fn parse_json(
    bytes: &[u8],
    mappings: &BTreeMap<String, TokenMapping>,
) -> Result<(Patch, bool), Error> {
    let Object(fields) = serde_json::from_slice(bytes).map_err(|_| Error::InvalidOutput)?;
    let mut patch = Patch::new();
    let mut truncated = false;
    for (token, mapping) in mappings {
        let (value, clipped) = match fields.get(&mapping.field) {
            Some(Value::Null) => (Token::Clear, false),
            Some(Value::String(s)) => render(s, mapping),
            Some(Value::Bool(v)) => render(&v.to_string(), mapping),
            Some(Value::Number(v)) => render(&v.to_string(), mapping),
            _ => return Err(Error::InvalidOutput),
        };
        truncated |= clipped;
        patch.insert(token.clone(), value);
    }
    Ok((patch, truncated))
}

fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    let mut plain = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            plain.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes.get(i).copied() {
            Some(b'[') => {
                i += 1;
                while i < bytes.len() {
                    let byte = bytes[i];
                    i += 1;
                    if (0x40..=0x7e).contains(&byte) {
                        break;
                    }
                }
            }
            Some(b']' | b'P' | b'X' | b'^' | b'_') => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            Some(_) => i += 1,
            None => {}
        }
    }
    plain
}

pub(super) fn parse_text(
    bytes: &[u8],
    mappings: &BTreeMap<String, TokenMapping>,
) -> Result<(Patch, bool), Error> {
    let plain = strip_ansi(bytes);
    let value = std::str::from_utf8(&plain).map_err(|_| Error::InvalidOutput)?;
    let mut patch = Patch::new();
    let mut truncated = false;
    for (token, mapping) in mappings {
        if mapping.field != "stdout" {
            return Err(Error::InvalidOutput);
        }
        let (value, clipped) = render(value, mapping);
        patch.insert(token.clone(), value);
        truncated |= clipped;
    }
    Ok((patch, truncated))
}
