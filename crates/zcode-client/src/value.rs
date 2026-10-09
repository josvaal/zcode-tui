//! Layer-1 serialization: the tagged binary format used by zcode's channel RPC.
//!
//! Wire format per value: `[1 byte type tag] [VQL length/count when applicable] [data]`.
//! VQL is a variable-length quantity: 7 data bits per byte, high bit marks continuation.

use serde_json::{Map, Number, Value as Json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Tag {
    Undefined = 0,
    String = 1,
    Buffer = 2,
    VsBuffer = 3,
    Array = 4,
    Object = 5,
    Int = 6,
}

/// A value that can travel through the channel protocol. Objects are carried
/// as JSON (the protocol's own fallback); binary payloads keep their own tags.
#[derive(Debug, Clone, PartialEq)]
pub enum RpcValue {
    Undefined,
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<RpcValue>),
    Object(Json),
    Int(i64),
}

impl RpcValue {
    pub fn from_json(v: Json) -> RpcValue {
        match v {
            Json::Null => RpcValue::Undefined,
            Json::Bool(b) => RpcValue::Object(Json::Bool(b)),
            Json::Number(n) => match n.as_i64() {
                Some(i) if i >= 0 && i <= u32::MAX as i64 => RpcValue::Int(i),
                _ => RpcValue::Object(Json::Number(n)),
            },
            Json::String(s) => RpcValue::String(s),
            Json::Array(items) => RpcValue::Array(items.into_iter().map(RpcValue::from_json).collect()),
            Json::Object(_) => RpcValue::Object(v),
        }
    }

    pub fn to_json(&self) -> Json {
        match self {
            RpcValue::Undefined => Json::Null,
            RpcValue::String(s) => Json::String(s.clone()),
            RpcValue::Bytes(b) => Json::Array(b.iter().map(|&x| Json::from(x)).collect()),
            RpcValue::Array(items) => Json::Array(items.iter().map(|v| v.to_json()).collect()),
            RpcValue::Object(o) => o.clone(),
            RpcValue::Int(i) => Json::Number(Number::from(*i)),
        }
    }
}

// ---------------------------------------------------------------------------
// VQL
// ---------------------------------------------------------------------------

pub fn read_vql(buf: &[u8], pos: &mut usize) -> Result<u32, String> {
    let mut value: u32 = 0;
    let mut n = 0;
    loop {
        let byte = *buf.get(*pos).ok_or("VQL read past end of buffer")?;
        *pos += 1;
        value |= ((byte & 0b0111_1111) as u32) << n;
        if byte & 0b1000_0000 == 0 {
            return Ok(value);
        }
        n += 7;
        if n > 31 {
            return Ok(value); // JS wraps at 32 bits via `|=`; follow suit
        }
    }
}

pub fn write_vql(out: &mut Vec<u8>, value: u32) {
    if value == 0 {
        out.push(0);
        return;
    }
    let mut v = value;
    while v != 0 {
        let mut byte = (v & 0b0111_1111) as u8;
        v >>= 7;
        if v > 0 {
            byte |= 0b1000_0000;
        }
        out.push(byte);
    }
}

// ---------------------------------------------------------------------------
// serialize / deserialize
// ---------------------------------------------------------------------------

pub fn serialize(value: &RpcValue, out: &mut Vec<u8>) {
    match value {
        RpcValue::Undefined => out.push(Tag::Undefined as u8),
        RpcValue::String(s) => {
            out.push(Tag::String as u8);
            write_vql(out, s.len() as u32);
            out.extend_from_slice(s.as_bytes());
        }
        RpcValue::Bytes(b) => {
            out.push(Tag::Buffer as u8);
            write_vql(out, b.len() as u32);
            out.extend_from_slice(b);
        }
        RpcValue::Array(items) => {
            out.push(Tag::Array as u8);
            write_vql(out, items.len() as u32);
            for item in items {
                serialize(item, out);
            }
        }
        RpcValue::Int(i) => {
            out.push(Tag::Int as u8);
            write_vql(out, *i as i32 as u32);
        }
        RpcValue::Object(o) => {
            let json = serde_json::to_string(o).expect("json serialize cannot fail");
            out.push(Tag::Object as u8);
            write_vql(out, json.len() as u32);
            out.extend_from_slice(json.as_bytes());
        }
    }
}

pub fn serialize_to_vec(value: &RpcValue) -> Vec<u8> {
    let mut out = Vec::new();
    serialize(value, &mut out);
    out
}

pub fn deserialize(buf: &[u8]) -> Result<RpcValue, String> {
    let mut pos = 0;
    let v = deserialize_at(buf, &mut pos)?;
    Ok(v)
}

fn deserialize_at(buf: &[u8], pos: &mut usize) -> Result<RpcValue, String> {
    let tag = *buf.get(*pos).ok_or("read past end")?;
    *pos += 1;
    match tag {
        0 => Ok(RpcValue::Undefined),
        1 => {
            let len = read_vql(buf, pos)? as usize;
            let s = std::str::from_utf8(slice(buf, pos, len)?)
                .map_err(|e| e.to_string())?
                .to_string();
            Ok(RpcValue::String(s))
        }
        2 | 3 => {
            let len = read_vql(buf, pos)? as usize;
            Ok(RpcValue::Bytes(slice(buf, pos, len)?.to_vec()))
        }
        4 => {
            let len = read_vql(buf, pos)?;
            let mut items = Vec::with_capacity(len.min(1024) as usize);
            for _ in 0..len {
                items.push(deserialize_at(buf, pos)?);
            }
            Ok(RpcValue::Array(items))
        }
        5 => {
            let len = read_vql(buf, pos)? as usize;
            let json: Json =
                serde_json::from_slice(slice(buf, pos, len)?).map_err(|e| e.to_string())?;
            Ok(RpcValue::Object(json))
        }
        6 => Ok(RpcValue::Int(read_vql(buf, pos)? as i32 as i64)),
        other => Err(format!("unknown type tag {other}")),
    }
}

fn slice<'a>(buf: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], String> {
    if *pos + len > buf.len() {
        return Err("read past end of buffer".into());
    }
    let s = &buf[*pos..*pos + len];
    *pos += len;
    Ok(s)
}

/// Convenience: a JSON object as an `RpcValue`.
pub fn obj(fields: &[(&str, Json)]) -> RpcValue {
    let mut map = Map::new();
    for (k, v) in fields {
        map.insert((*k).to_string(), v.clone());
    }
    RpcValue::Object(Json::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn vql_roundtrip() {
        for v in [0u32, 1, 127, 128, 300, u32::MAX] {
            let mut out = Vec::new();
            write_vql(&mut out, v);
            let mut pos = 0;
            assert_eq!(read_vql(&out, &mut pos).unwrap(), v);
            assert_eq!(pos, out.len());
        }
    }

    #[test]
    fn roundtrip_all_types() {
        let v = RpcValue::Array(vec![
            RpcValue::Undefined,
            RpcValue::String("hola ñ".into()),
            RpcValue::Bytes(vec![0xde, 0xad]),
            RpcValue::Int(42),
            RpcValue::Int(0),
            obj(&[("k", json!({"nested": [1, "x", null]}))]),
        ]);
        let bytes = serialize_to_vec(&v);
        assert_eq!(deserialize(&bytes).unwrap(), v);
    }

    #[test]
    fn vql_examples_from_ts_impl() {
        // 0 -> [0x00], 127 -> [0x7F], 128 -> [0x80, 0x01]
        let mut out = Vec::new();
        write_vql(&mut out, 0);
        assert_eq!(out, vec![0x00]);
        out.clear();
        write_vql(&mut out, 127);
        assert_eq!(out, vec![0x7F]);
        out.clear();
        write_vql(&mut out, 128);
        assert_eq!(out, vec![0x80, 0x01]);
    }
}
