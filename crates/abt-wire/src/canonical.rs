//! Canonical forms for hashing (P-6): RFC 8785 (JCS) JSON rendered from the
//! owned types. Object members are sorted by their names as UTF-16 code
//! units, there is no whitespace, a float is written in the ECMAScript
//! shortest round-trip form (`Number.prototype.toString`), and an integer
//! (`i64`, `u64`) as its exact decimal digits. A NaN or an infinity is an
//! error at construction, never `null`. The wire bytes are never hashed.

use std::fmt;

use serde::ser::{self, Serialize};

use crate::hash::Hash;

/// What has a canonical form, and therefore a hash.
pub trait Canonical {
    fn canonical(&self) -> Result<String, CanonicalError>;

    fn canonical_hash(&self) -> Result<Hash, CanonicalError> {
        Ok(Hash::of(self.canonical()?.as_bytes()))
    }
}

impl<T: Serialize + ?Sized> Canonical for T {
    fn canonical(&self) -> Result<String, CanonicalError> {
        to_canonical_json(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalError(pub String);

impl fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CanonicalError {}

impl ser::Error for CanonicalError {
    fn custom<M: fmt::Display>(m: M) -> Self {
        CanonicalError(m.to_string())
    }
}

/// The canonical JSON text of a value.
pub fn to_canonical_json<T: Serialize + ?Sized>(v: &T) -> Result<String, CanonicalError> {
    let tree = v.serialize(Tree)?;
    let mut out = String::new();
    write(&tree, &mut out);
    Ok(out)
}

/// The canonical form of an arbitrary JSON text (its members re-sorted, its
/// numbers re-rendered); what `abt hash --show` applies to a JSON input.
pub fn canonicalize_json_text(text: &str) -> Result<String, CanonicalError> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| CanonicalError(e.to_string()))?;
    to_canonical_json(&v)
}

/// A JSON value with its integers kept exact.
#[derive(Clone, Debug, PartialEq)]
enum Node {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(String),
    Arr(Vec<Node>),
    Obj(Vec<(String, Node)>),
}

fn write(n: &Node, out: &mut String) {
    match n {
        Node::Null => out.push_str("null"),
        Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Node::I64(i) => out.push_str(&i.to_string()),
        Node::U64(u) => out.push_str(&u.to_string()),
        Node::F64(x) => out.push_str(&es_number(*x)),
        Node::Str(s) => write_string(s, out),
        Node::Arr(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(x, out);
            }
            out.push(']');
        }
        Node::Obj(members) => {
            let mut sorted: Vec<&(String, Node)> = members.iter().collect();
            sorted.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            out.push('{');
            for (i, (k, v)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write(v, out);
            }
            out.push('}');
        }
    }
}

/// A string as ECMAScript's `JSON.stringify` writes it (RFC 8785, 3.2.2.2).
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A finite double as ECMAScript's `Number.prototype.toString` writes it:
/// the shortest digits that round-trip, in positional notation from 1e-6 to
/// below 1e21 and in exponent notation (`1e+21`, `1.5e-7`) outside; `-0` is
/// `0` (RFC 8785, 3.2.2.3).
pub fn es_number(x: f64) -> String {
    debug_assert!(x.is_finite());
    if x == 0.0 {
        return "0".to_string();
    }
    // Rust's `{:e}` writes the shortest round-trip digits: d[.ddd]e±x.
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').expect("LowerExp writes an exponent");
    let exp: i32 = exp.parse().expect("an integer exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    let body = if k <= n && n <= 21 {
        format!("{}{}", digits, "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{}", "0".repeat((-n) as usize), digits)
    } else {
        let e = n - 1;
        let sign = if e >= 0 { '+' } else { '-' };
        if k == 1 {
            format!("{}e{}{}", digits, sign, e.abs())
        } else {
            format!("{}.{}e{}{}", &digits[..1], &digits[1..], sign, e.abs())
        }
    };
    if x < 0.0 {
        format!("-{}", body)
    } else {
        body
    }
}

/// A serde serializer into [`Node`].
struct Tree;

fn float(x: f64) -> Result<Node, CanonicalError> {
    if x.is_finite() {
        Ok(Node::F64(x))
    } else {
        Err(CanonicalError(format!("{} has no canonical form (P-6: NaN and infinities are refused at construction)", x)))
    }
}

impl ser::Serializer for Tree {
    type Ok = Node;
    type Error = CanonicalError;
    type SerializeSeq = SeqBuilder;
    type SerializeTuple = SeqBuilder;
    type SerializeTupleStruct = SeqBuilder;
    type SerializeTupleVariant = VariantSeqBuilder;
    type SerializeMap = MapBuilder;
    type SerializeStruct = MapBuilder;
    type SerializeStructVariant = VariantMapBuilder;

    fn serialize_bool(self, v: bool) -> Result<Node, CanonicalError> {
        Ok(Node::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<Node, CanonicalError> {
        Ok(Node::I64(v as i64))
    }
    fn serialize_i16(self, v: i16) -> Result<Node, CanonicalError> {
        Ok(Node::I64(v as i64))
    }
    fn serialize_i32(self, v: i32) -> Result<Node, CanonicalError> {
        Ok(Node::I64(v as i64))
    }
    fn serialize_i64(self, v: i64) -> Result<Node, CanonicalError> {
        Ok(Node::I64(v))
    }
    fn serialize_i128(self, v: i128) -> Result<Node, CanonicalError> {
        i64::try_from(v).map(Node::I64).map_err(|_| CanonicalError(format!("{} is outside i64", v)))
    }
    fn serialize_u8(self, v: u8) -> Result<Node, CanonicalError> {
        Ok(Node::U64(v as u64))
    }
    fn serialize_u16(self, v: u16) -> Result<Node, CanonicalError> {
        Ok(Node::U64(v as u64))
    }
    fn serialize_u32(self, v: u32) -> Result<Node, CanonicalError> {
        Ok(Node::U64(v as u64))
    }
    fn serialize_u64(self, v: u64) -> Result<Node, CanonicalError> {
        Ok(Node::U64(v))
    }
    fn serialize_u128(self, v: u128) -> Result<Node, CanonicalError> {
        u64::try_from(v).map(Node::U64).map_err(|_| CanonicalError(format!("{} is outside u64", v)))
    }
    fn serialize_f32(self, v: f32) -> Result<Node, CanonicalError> {
        float(v as f64)
    }
    fn serialize_f64(self, v: f64) -> Result<Node, CanonicalError> {
        float(v)
    }
    fn serialize_char(self, v: char) -> Result<Node, CanonicalError> {
        Ok(Node::Str(v.to_string()))
    }
    fn serialize_str(self, v: &str) -> Result<Node, CanonicalError> {
        Ok(Node::Str(v.to_string()))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<Node, CanonicalError> {
        Ok(Node::Arr(v.iter().map(|b| Node::U64(*b as u64)).collect()))
    }
    fn serialize_none(self) -> Result<Node, CanonicalError> {
        Ok(Node::Null)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<Node, CanonicalError> {
        v.serialize(Tree)
    }
    fn serialize_unit(self) -> Result<Node, CanonicalError> {
        Ok(Node::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Node, CanonicalError> {
        Ok(Node::Null)
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, variant: &'static str) -> Result<Node, CanonicalError> {
        Ok(Node::Str(variant.to_string()))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, v: &T) -> Result<Node, CanonicalError> {
        v.serialize(Tree)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _: &'static str, _: u32, variant: &'static str, v: &T) -> Result<Node, CanonicalError> {
        Ok(Node::Obj(vec![(variant.to_string(), v.serialize(Tree)?)]))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<SeqBuilder, CanonicalError> {
        Ok(SeqBuilder(Vec::with_capacity(len.unwrap_or(0))))
    }
    fn serialize_tuple(self, len: usize) -> Result<SeqBuilder, CanonicalError> {
        Ok(SeqBuilder(Vec::with_capacity(len)))
    }
    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<SeqBuilder, CanonicalError> {
        Ok(SeqBuilder(Vec::with_capacity(len)))
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, variant: &'static str, len: usize) -> Result<VariantSeqBuilder, CanonicalError> {
        Ok(VariantSeqBuilder(variant, Vec::with_capacity(len)))
    }
    fn serialize_map(self, len: Option<usize>) -> Result<MapBuilder, CanonicalError> {
        Ok(MapBuilder(Vec::with_capacity(len.unwrap_or(0)), None))
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<MapBuilder, CanonicalError> {
        Ok(MapBuilder(Vec::with_capacity(len), None))
    }
    fn serialize_struct_variant(self, _: &'static str, _: u32, variant: &'static str, len: usize) -> Result<VariantMapBuilder, CanonicalError> {
        Ok(VariantMapBuilder(variant, Vec::with_capacity(len)))
    }
}

struct SeqBuilder(Vec<Node>);
struct VariantSeqBuilder(&'static str, Vec<Node>);
struct MapBuilder(Vec<(String, Node)>, Option<String>);
struct VariantMapBuilder(&'static str, Vec<(String, Node)>);

impl ser::SerializeSeq for SeqBuilder {
    type Ok = Node;
    type Error = CanonicalError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), CanonicalError> {
        self.0.push(v.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Node, CanonicalError> {
        Ok(Node::Arr(self.0))
    }
}

impl ser::SerializeTuple for SeqBuilder {
    type Ok = Node;
    type Error = CanonicalError;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), CanonicalError> {
        self.0.push(v.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Node, CanonicalError> {
        Ok(Node::Arr(self.0))
    }
}

impl ser::SerializeTupleStruct for SeqBuilder {
    type Ok = Node;
    type Error = CanonicalError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), CanonicalError> {
        self.0.push(v.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Node, CanonicalError> {
        Ok(Node::Arr(self.0))
    }
}

impl ser::SerializeTupleVariant for VariantSeqBuilder {
    type Ok = Node;
    type Error = CanonicalError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), CanonicalError> {
        self.1.push(v.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Node, CanonicalError> {
        Ok(Node::Obj(vec![(self.0.to_string(), Node::Arr(self.1))]))
    }
}

/// A map key must render as a string: a string, a character or an integer.
fn key_text(n: Node) -> Result<String, CanonicalError> {
    match n {
        Node::Str(s) => Ok(s),
        Node::I64(i) => Ok(i.to_string()),
        Node::U64(u) => Ok(u.to_string()),
        Node::Bool(b) => Ok(b.to_string()),
        other => Err(CanonicalError(format!("a map key must be a string or an integer, not {:?}", other))),
    }
}

impl ser::SerializeMap for MapBuilder {
    type Ok = Node;
    type Error = CanonicalError;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, k: &T) -> Result<(), CanonicalError> {
        self.1 = Some(key_text(k.serialize(Tree)?)?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), CanonicalError> {
        let k = self.1.take().ok_or_else(|| CanonicalError("a map value without its key".into()))?;
        self.0.push((k, v.serialize(Tree)?));
        Ok(())
    }
    fn end(self) -> Result<Node, CanonicalError> {
        duplicate_free(self.0)
    }
}

impl ser::SerializeStruct for MapBuilder {
    type Ok = Node;
    type Error = CanonicalError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, v: &T) -> Result<(), CanonicalError> {
        self.0.push((key.to_string(), v.serialize(Tree)?));
        Ok(())
    }
    fn end(self) -> Result<Node, CanonicalError> {
        duplicate_free(self.0)
    }
}

impl ser::SerializeStructVariant for VariantMapBuilder {
    type Ok = Node;
    type Error = CanonicalError;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, v: &T) -> Result<(), CanonicalError> {
        self.1.push((key.to_string(), v.serialize(Tree)?));
        Ok(())
    }
    fn end(self) -> Result<Node, CanonicalError> {
        Ok(Node::Obj(vec![(self.0.to_string(), duplicate_free(self.1)?)]))
    }
}

fn duplicate_free(members: Vec<(String, Node)>) -> Result<Node, CanonicalError> {
    let mut names: Vec<&str> = members.iter().map(|(k, _)| k.as_str()).collect();
    names.sort_unstable();
    if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
        return Err(CanonicalError(format!("the member `{}` appears twice", w[0])));
    }
    Ok(Node::Obj(members))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_render_as_ecmascript_does() {
        // RFC 8785, appendix B, and ECMA-262's Number::toString boundaries.
        let cases: [(f64, &str); 14] = [
            (0.0, "0"),
            (-0.0, "0"),
            (f64::from_bits(0x0000000000000001), "5e-324"),
            (f64::from_bits(0x8000000000000001), "-5e-324"),
            (f64::from_bits(0x7fefffffffffffff), "1.7976931348623157e+308"),
            (f64::from_bits(0x4340000000000000), "9007199254740992"),
            (f64::from_bits(0x44b52d02c7e14af6), "1e+23"),
            (f64::from_bits(0x3eb0c6f7a0b5ed8d), "0.000001"),
            (f64::from_bits(0x41b3de4355555555), "333333333.3333333"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (1e-7, "1e-7"),
            (4.5, "4.5"),
            (-0.002, "-0.002"),
        ];
        for (x, want) in cases {
            assert_eq!(es_number(x), want, "{:e}", x);
        }
    }

    #[test]
    fn members_sort_by_utf16_code_units() {
        // RFC 8785, section 3.2.3: the surrogate pair of U+1F600 sorts before U+FB33.
        let text =
            r#"{"€":"Euro Sign","\r":"Carriage Return","דּ":"Hebrew Letter Dalet With Dagesh","1":"One","😀":"Emoji: Grinning Face","\u0080":"Control","ö":"Latin Small Letter O With Diaeresis"}"#;
        let got = canonicalize_json_text(text).unwrap();
        let keys: Vec<String> = {
            let v: serde_json::Value = serde_json::from_str(&got).unwrap();
            let _ = v;
            let mut out = Vec::new();
            let mut rest = got.as_str();
            while let Some(i) = rest.find("\":\"") {
                let start = rest[..i].rfind(['{', ',']).unwrap() + 2;
                out.push(rest[start..i].to_string());
                rest = &rest[i + 3..];
            }
            out
        };
        assert_eq!(keys, vec!["\\r", "1", "\u{80}", "\u{f6}", "\u{20ac}", "\u{1f600}", "\u{fb33}"]);
    }

    #[test]
    fn non_finite_numbers_are_refused() {
        assert!(to_canonical_json(&f64::NAN).is_err());
        assert!(to_canonical_json(&vec![1.0, f64::INFINITY]).is_err());
        assert_eq!(to_canonical_json(&vec![1.0, 2.5]).unwrap(), "[1,2.5]");
    }

    #[test]
    fn integers_are_exact_and_strings_escaped() {
        assert_eq!(to_canonical_json(&u64::MAX).unwrap(), "18446744073709551615");
        assert_eq!(to_canonical_json(&i64::MIN).unwrap(), "-9223372036854775808");
        assert_eq!(to_canonical_json("a\"b\\c\u{1}\n").unwrap(), "\"a\\\"b\\\\c\\u0001\\n\"");
    }
}
