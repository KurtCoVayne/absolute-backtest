//! Identifiers (A-6): 32-byte BLAKE3 hashes, printed as hex. A hash is taken
//! over canonical bytes (P-6), never over wire bytes.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A BLAKE3 hash.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    pub fn of(bytes: &[u8]) -> Hash {
        Hash(*blake3::hash(bytes).as_bytes())
    }

    /// The hash of several parts, each length-prefixed so that no two lists
    /// of parts share their concatenation.
    pub fn of_parts<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> Hash {
        let mut h = blake3::Hasher::new();
        for p in parts {
            h.update(&(p.len() as u64).to_le_bytes());
            h.update(p);
        }
        Hash(*h.finalize().as_bytes())
    }

    pub fn hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }

    pub fn from_hex(s: &str) -> Result<Hash, HashParseError> {
        if s.len() != 64 {
            return Err(HashParseError(format!("a hash is 64 hex digits, `{}` has {}", s, s.len())));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let pair = std::str::from_utf8(chunk).map_err(|_| HashParseError(format!("`{}` is not hex", s)))?;
            out[i] = u8::from_str_radix(pair, 16).map_err(|_| HashParseError(format!("`{}` is not hex", s)))?;
        }
        Ok(Hash(out))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashParseError(pub String);

impl fmt::Display for HashParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HashParseError {}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", self.hex())
    }
}

impl FromStr for Hash {
    type Err = HashParseError;
    fn from_str(s: &str) -> Result<Hash, HashParseError> {
        Hash::from_hex(s)
    }
}

impl Serialize for Hash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Hash, D::Error> {
        let s = String::deserialize(d)?;
        Hash::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

macro_rules! hash_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub crate::hash::Hash);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

hash_id!(
    /// A relation version: the hash of its immutable dataset's bytes (D-2.6).
    VersionId
);
hash_id!(
    /// A binding: the sorted (relation, version) pairs, the parameter point
    /// and the dynamic-demand filters (D-7.5).
    BindingId
);
hash_id!(
    /// A run: (H(P), β, π, mode) (Definition A-4.1).
    RunId
);
hash_id!(
    /// A plan: its normal form (Q-2.4).
    PlanHash
);
hash_id!(
    /// A trial: (H(Π), step, run id) (Q-2.5).
    TrialId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_matches_blake3() {
        // The BLAKE3 hash of the empty input (the reference test vector).
        let h = Hash::of(b"");
        assert_eq!(h.hex(), "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
        assert_eq!(Hash::from_hex(&h.hex()).unwrap(), h);
        assert!(Hash::from_hex("xyz").is_err());
        assert_ne!(Hash::of_parts([b"ab".as_slice(), b"c"]), Hash::of_parts([b"a".as_slice(), b"bc"]));
    }
}
