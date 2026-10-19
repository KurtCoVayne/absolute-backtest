//! Known-answer tests of the DBN decoder and encoder (`abt_data::adapters::dbn`,
//! D-7.7).
//!
//! The fixtures (`tests/fixtures/dbn`) are DBN files written by the reference
//! implementation, Databento's `dbn` crate 0.72.0, each with the reference
//! decoder's view of its metadata and records in `expected/<name>.json`
//! (u64 and i64 values beyond 2^53 as strings). They cover the three
//! versions of OHLCV bars, instrument definitions, statistics, and the live
//! gateway's symbol mappings, errors and system messages, a live stream with
//! `ts_out`, and records in which every byte depends on its offset, so a
//! field read from the wrong place is a mismatch. This file uses only the
//! module under test and `serde_json`.

use abt_data::adapters::dbn::{decode, encode, rtype, DbnError, Decoded, Decoder, Header, InstrumentDef, Metadata, Record, SYMBOL_CSTR_LEN_V2};

/// The fixture directory, relative to the manifest directory of the crate under test.
const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dbn");

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use serde_json::{json, Map, Value};

struct Fixture {
    name: String,
    bytes: Vec<u8>,
    expected: Value,
}

fn fixture(name: &str) -> Fixture {
    let dir = Path::new(FIXTURE_DIR);
    let bytes = fs::read(dir.join(format!("{name}.dbn"))).unwrap_or_else(|e| panic!("reading fixture {name}: {e}"));
    let json = fs::read_to_string(dir.join("expected").join(format!("{name}.json"))).unwrap_or_else(|e| panic!("reading the expected values of {name}: {e}"));
    Fixture {
        name: name.to_owned(),
        bytes,
        expected: serde_json::from_str(&json).unwrap(),
    }
}

/// Returns every fixture in `FIXTURE_DIR`, sorted by name.
fn fixtures() -> Vec<Fixture> {
    let entries = fs::read_dir(FIXTURE_DIR).unwrap_or_else(|e| panic!("reading {FIXTURE_DIR}: {e}"));
    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension()? == "dbn").then(|| path.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    assert!(names.len() >= 16, "expected at least 16 fixtures in {FIXTURE_DIR}, found {names:?}");
    names.iter().map(|name| fixture(name)).collect()
}

// ---------------------------------------------------------------------------------------------------
// The JSON view of decoded data, in the shape of the expected files

/// Returns an integer as a JSON number, or as a decimal string if its magnitude exceeds 2^53.
fn n<T: Into<i128>>(x: T) -> Value {
    let x: i128 = x.into();
    if x.unsigned_abs() > 1_u128 << 53 {
        Value::from(x.to_string())
    } else if x < 0 {
        Value::from(x as i64)
    } else {
        Value::from(x as u64)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn merge(m: &mut Map<String, Value>, fields: Value) {
    if let Value::Object(fields) = fields {
        m.extend(fields);
    }
}

macro_rules! nums {
    ($m:ident, $r:expr, $($f:ident),* $(,)?) => { $( $m.insert(stringify!($f).to_owned(), n($r.$f)); )* };
}
macro_rules! strs {
    ($m:ident, $r:expr, $($f:ident),* $(,)?) => { $( $m.insert(stringify!($f).to_owned(), Value::from($r.$f.as_str())); )* };
}

fn metadata_view(m: &Metadata) -> Value {
    let mappings: Vec<Value> = m
        .mappings
        .iter()
        .map(|mapping| {
            let intervals: Vec<Value> = mapping
                .intervals
                .iter()
                .map(|i| json!({ "start_date": i.start_date, "end_date": i.end_date, "symbol": i.symbol }))
                .collect();
            json!({ "raw_symbol": mapping.raw_symbol, "intervals": intervals })
        })
        .collect();
    json!({
        "version": m.version,
        "dataset": m.dataset,
        "schema": m.schema,
        "start": n(m.start),
        "end": m.end.map(n),
        "limit": m.limit.map(n),
        "stype_in": m.stype_in,
        "stype_out": m.stype_out,
        "ts_out": m.ts_out,
        "symbol_cstr_len": m.symbol_cstr_len,
        "symbols": m.symbols,
        "partial": m.partial,
        "not_found": m.not_found,
        "mappings": mappings,
    })
}

fn record_view(decoded: &Decoded) -> Value {
    let mut m = Map::new();
    let (kind, hd) = match &decoded.record {
        Record::Ohlcv { hd, open, high, low, close, volume } => {
            merge(&mut m, json!({ "open": n(*open), "high": n(*high), "low": n(*low), "close": n(*close), "volume": n(*volume) }));
            ("ohlcv", hd)
        }
        Record::InstrumentDef(d) => {
            nums!(
                m,
                d,
                ts_recv,
                min_price_increment,
                display_factor,
                expiration,
                activation,
                high_limit_price,
                low_limit_price,
                max_price_variation,
                trading_reference_price,
                unit_of_measure_qty,
                min_price_increment_amount,
                price_ratio,
                strike_price,
                raw_instrument_id,
                leg_price,
                leg_delta,
                inst_attrib_value,
                underlying_id,
                market_depth_implied,
                market_depth,
                market_segment_id,
                max_trade_vol,
                min_lot_size,
                min_lot_size_block,
                min_lot_size_round_lot,
                min_trade_vol,
                contract_multiplier,
                decay_quantity,
                original_contract_size,
                leg_instrument_id,
                leg_ratio_price_numerator,
                leg_ratio_price_denominator,
                leg_ratio_qty_numerator,
                leg_ratio_qty_denominator,
                leg_underlying_id,
                trading_reference_date,
                appl_id,
                maturity_year,
                decay_start_date,
                channel_id,
                leg_count,
                leg_index,
                instrument_class,
                match_algorithm,
                md_security_trading_status,
                main_fraction,
                price_display_format,
                settl_price_type,
                sub_fraction,
                underlying_product,
                security_update_action,
                maturity_month,
                maturity_day,
                maturity_week,
                user_defined_instrument,
                contract_multiplier_unit,
                flow_schedule_type,
                tick_rule,
                leg_instrument_class,
                leg_side
            );
            strs!(
                m,
                d,
                currency,
                settl_currency,
                secsubtype,
                raw_symbol,
                group,
                exchange,
                asset,
                cfi,
                security_type,
                unit_of_measure,
                underlying,
                strike_price_currency,
                leg_raw_symbol
            );
            ("instrument_def", &d.hd)
        }
        Record::Statistics {
            hd,
            ts_recv,
            ts_ref,
            price,
            quantity,
            sequence,
            ts_in_delta,
            stat_type,
            channel_id,
            update_action,
            stat_flags,
        } => {
            merge(
                &mut m,
                json!({
                    "ts_recv": n(*ts_recv), "ts_ref": n(*ts_ref), "price": n(*price), "quantity": n(*quantity), "sequence": n(*sequence),
                    "ts_in_delta": n(*ts_in_delta), "stat_type": n(*stat_type), "channel_id": n(*channel_id), "update_action": n(*update_action),
                    "stat_flags": n(*stat_flags),
                }),
            );
            ("statistics", hd)
        }
        Record::SymbolMapping {
            hd,
            stype_in,
            stype_in_symbol,
            stype_out,
            stype_out_symbol,
            start_ts,
            end_ts,
        } => {
            merge(
                &mut m,
                json!({
                    "stype_in": stype_in, "stype_in_symbol": stype_in_symbol, "stype_out": stype_out, "stype_out_symbol": stype_out_symbol,
                    "start_ts": n(*start_ts), "end_ts": n(*end_ts),
                }),
            );
            ("symbol_mapping", hd)
        }
        Record::Error { hd, err, code, is_last } => {
            merge(&mut m, json!({ "err": err, "code": code, "is_last": is_last }));
            ("error", hd)
        }
        Record::System { hd, msg, code } => {
            merge(&mut m, json!({ "msg": msg, "code": code }));
            ("system", hd)
        }
        Record::Other { hd, body } => {
            m.insert("body".to_owned(), Value::from(hex(body)));
            ("other", hd)
        }
    };
    m.insert("kind".to_owned(), Value::from(kind));
    m.insert(
        "hd".to_owned(),
        json!({ "rtype": hd.rtype, "publisher_id": hd.publisher_id, "instrument_id": hd.instrument_id, "ts_event": n(hd.ts_event) }),
    );
    m.insert("ts_out".to_owned(), decoded.ts_out.map_or(Value::Null, n));
    Value::Object(m)
}

/// Panics with the differing fields if `actual` and `expected` differ.
fn assert_same(actual: &Value, expected: &Value, context: &str) {
    if actual == expected {
        return;
    }
    let mut lines = Vec::new();
    if let (Some(a), Some(e)) = (actual.as_object(), expected.as_object()) {
        for key in a.keys().chain(e.keys()).collect::<BTreeSet<_>>() {
            if a.get(key) != e.get(key) {
                lines.push(format!("  {key}: decoded {:?}, expected {:?}", a.get(key), e.get(key)));
            }
        }
    } else {
        lines.push(format!("  decoded {actual}, expected {expected}"));
    }
    panic!("{context} differs from the reference:\n{}", lines.join("\n"));
}

// ---------------------------------------------------------------------------------------------------
// Helpers

/// Returns the offsets of the end of the metadata and of the end of each record.
fn boundaries(bytes: &[u8]) -> Vec<usize> {
    let mut pos = 8 + u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let mut ends = vec![pos];
    while pos < bytes.len() {
        pos += usize::from(bytes[pos]) * 4;
        ends.push(pos);
    }
    assert_eq!(pos, bytes.len(), "the records end exactly at the end of the input");
    ends
}

/// Decodes with the streaming decoder until the end of the input or the first error.
fn decode_stream<R: Read>(reader: R) -> (Vec<Decoded>, Result<(), DbnError>) {
    let mut decoder = match Decoder::new(reader) {
        Ok(decoder) => decoder,
        Err(err) => return (Vec::new(), Err(err)),
    };
    let mut records = Vec::new();
    loop {
        match decoder.next() {
            Ok(Some(record)) => records.push(record),
            Ok(None) => return (records, Ok(())),
            Err(err) => return (records, Err(err)),
        }
    }
}

/// A reader that returns at most `chunk` bytes per call.
struct Trickle<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.chunk.min(buf.len()).min(self.data.len());
        buf[..len].copy_from_slice(&self.data[..len]);
        self.data = &self.data[len..];
        Ok(len)
    }
}

/// A reader that fails every third call with `Interrupted` and, once `calm` bytes have been read, every
/// other third call with `WouldBlock`, like a non-blocking socket; the other calls return one to three bytes.
struct Flaky<'a> {
    data: &'a [u8],
    pos: usize,
    calm: usize,
    calls: usize,
}

impl Read for Flaky<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        match self.calls % 3 {
            0 => return Err(io::ErrorKind::Interrupted.into()),
            1 if self.pos >= self.calm => return Err(io::ErrorKind::WouldBlock.into()),
            _ => {}
        }
        let len = (1 + self.calls % 4).min(buf.len()).min(self.data.len() - self.pos);
        buf[..len].copy_from_slice(&self.data[self.pos..self.pos + len]);
        self.pos += len;
        Ok(len)
    }
}

fn first_def(records: &[Decoded]) -> InstrumentDef {
    records
        .iter()
        .find_map(|d| match &d.record {
            Record::InstrumentDef(def) => Some((**def).clone()),
            _ => None,
        })
        .unwrap()
}

/// Returns the position of the only occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> usize {
    let positions: Vec<usize> = haystack.windows(needle.len()).enumerate().filter(|(_, w)| *w == needle).map(|(i, _)| i).collect();
    assert_eq!(positions.len(), 1, "{needle:?} occurs once");
    positions[0]
}

// ---------------------------------------------------------------------------------------------------
// Known answers

#[test]
fn every_fixture_decodes_to_the_reference_values() {
    for f in fixtures() {
        let (metadata, records) = decode(&f.bytes).unwrap_or_else(|e| panic!("{}: {e}", f.name));
        assert_same(&metadata_view(&metadata), &f.expected["metadata"], &format!("{}: the metadata", f.name));
        let expected = f.expected["records"].as_array().unwrap();
        assert_eq!(records.len(), expected.len(), "{}: the number of records", f.name);
        for (i, (record, expected)) in records.iter().zip(expected).enumerate() {
            assert_same(&record_view(record), expected, &format!("{}: record {i}", f.name));
        }
    }
}

#[test]
fn encoding_the_decoded_fixtures_reproduces_them() {
    for f in fixtures() {
        let (metadata, records) = decode(&f.bytes).unwrap();
        let encoded = encode(&metadata, &records);
        if let Some(pos) = encoded.iter().zip(&f.bytes).position(|(a, b)| a != b) {
            panic!("{}: the encoding differs from the fixture at byte {pos} of {}", f.name, f.bytes.len());
        }
        assert_eq!(encoded.len(), f.bytes.len(), "{}: the length of the encoding", f.name);
    }
}

#[test]
fn the_fixtures_cover_every_record_type_in_every_version() {
    let kind = |record: &Record| match record {
        Record::Ohlcv { .. } => "ohlcv",
        Record::InstrumentDef(_) => "instrument_def",
        Record::Statistics { .. } => "statistics",
        Record::SymbolMapping { .. } => "symbol_mapping",
        Record::Error { .. } => "error",
        Record::System { .. } => "system",
        Record::Other { .. } => "other",
    };
    let mut seen = BTreeSet::new();
    let mut ohlcv_rtypes = BTreeSet::new();
    for f in fixtures() {
        let (metadata, records) = decode(&f.bytes).unwrap();
        for d in &records {
            seen.insert((metadata.version, kind(&d.record), d.ts_out.is_some()));
            if let Record::Ohlcv { hd, .. } = &d.record {
                ohlcv_rtypes.insert(hd.rtype);
            }
        }
    }
    let kinds = ["ohlcv", "instrument_def", "statistics", "symbol_mapping", "error", "system", "other"];
    for version in 1..=3 {
        for kind in kinds {
            assert!(seen.contains(&(version, kind, false)), "no {kind} record in a version {version} fixture");
        }
    }
    for kind in kinds {
        assert!(seen.contains(&(3, kind, true)), "no {kind} record with ts_out");
    }
    let all_ohlcv = [rtype::OHLCV_DEPRECATED, rtype::OHLCV_1S, rtype::OHLCV_1M, rtype::OHLCV_1H, rtype::OHLCV_1D, rtype::OHLCV_EOD];
    assert_eq!(ohlcv_rtypes, BTreeSet::from(all_ohlcv));
}

// ---------------------------------------------------------------------------------------------------
// Streaming and truncation

#[test]
fn streaming_one_byte_at_a_time_gives_the_same_records() {
    for f in fixtures() {
        let (metadata, records) = decode(&f.bytes).unwrap();
        let mut decoder = Decoder::new(Trickle { data: &f.bytes, chunk: 1 }).unwrap();
        assert_eq!(decoder.metadata(), &metadata, "{}", f.name);
        let mut streamed = Vec::new();
        while let Some(record) = decoder.next().unwrap() {
            streamed.push(record);
        }
        assert_eq!(streamed, records, "{}", f.name);
        assert!(decoder.next().unwrap().is_none(), "{}: the end of the input stays the end", f.name);
    }
}

#[test]
fn streaming_resumes_after_would_block_and_retries_interrupted_reads() {
    for f in fixtures() {
        let (_, records) = decode(&f.bytes).unwrap();
        let calm = boundaries(&f.bytes)[0];
        let mut decoder = Decoder::new(Flaky {
            data: &f.bytes,
            pos: 0,
            calm,
            calls: 0,
        })
        .unwrap();
        let mut streamed = Vec::new();
        let mut blocked = 0;
        loop {
            match decoder.next() {
                Ok(Some(record)) => streamed.push(record),
                Ok(None) => break,
                Err(DbnError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => blocked += 1,
                Err(err) => panic!("{}: {err}", f.name),
            }
        }
        assert_eq!(streamed, records, "{}", f.name);
        assert!(blocked > records.len(), "{}: the reader blocked inside records", f.name);
    }
}

#[test]
fn truncation_at_every_offset_ends_cleanly_or_reports_truncation() {
    for f in fixtures() {
        let (_, records) = decode(&f.bytes).unwrap();
        let ends = boundaries(&f.bytes);
        for cut in 0..=f.bytes.len() {
            let (got, end) = decode_stream(&f.bytes[..cut]);
            if cut < ends[0] {
                assert!(matches!(end, Err(DbnError::Truncated { .. })), "{}: cut at {cut} in the metadata gives {end:?}", f.name);
                continue;
            }
            let complete = ends.iter().filter(|&&end| end <= cut).count() - 1;
            assert_eq!(got, records[..complete], "{}: cut at {cut}", f.name);
            match end {
                Ok(()) => assert!(ends.contains(&cut), "{}: cut at {cut} inside a record ends cleanly", f.name),
                Err(DbnError::Truncated { offset, available, .. }) => {
                    assert!(!ends.contains(&cut), "{}: cut at {cut} on a record boundary reports truncation", f.name);
                    assert_eq!(offset as usize, ends[complete], "{}: cut at {cut}", f.name);
                    assert_eq!(available, cut - ends[complete], "{}: cut at {cut}", f.name);
                }
                Err(err) => panic!("{}: cut at {cut} gives {err}", f.name),
            }
            let whole = decode(&f.bytes[..cut]);
            assert_eq!(whole.is_ok(), ends.contains(&cut), "{}: decode of the first {cut} bytes", f.name);
        }
    }
}

/// A xorshift generator, for reproducible corruption.
struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn corrupted_input_never_panics_and_decodes_consistently() {
    let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15);
    for f in fixtures() {
        for _ in 0..300 {
            let mut bytes = f.bytes.clone();
            for _ in 0..1 + rng.next() % 4 {
                let pos = (rng.next() % bytes.len() as u64) as usize;
                bytes[pos] = rng.next() as u8;
            }
            let (streamed, end) = decode_stream(Trickle { data: &bytes, chunk: 7 });
            match decode(&bytes) {
                Ok((metadata, records)) => {
                    assert!(end.is_ok(), "{}: the streaming decoder fails where decode succeeds", f.name);
                    assert_eq!(streamed, records, "{}", f.name);
                    // Whatever decodes, encodes to a stream that decodes to the same values.
                    let reencoded = encode(&metadata, &records);
                    assert_eq!(decode(&reencoded).unwrap(), (metadata, records), "{}", f.name);
                }
                Err(_) => assert!(end.is_err(), "{}: the streaming decoder succeeds where decode fails", f.name),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Malformed input

#[test]
fn input_that_is_not_dbn_of_versions_1_to_3_is_rejected() {
    let mut bytes = fixture("ohlcv_1m_v3").bytes;
    bytes[2] = b'Z';
    assert!(matches!(decode(&bytes), Err(DbnError::BadMagic { found }) if &found == b"DBZ"));
    let zstd = [0x28, 0xB5, 0x2F, 0xFD, 0, 0, 0, 0];
    assert!(matches!(decode(&zstd), Err(DbnError::BadMagic { .. })));
    for version in [0, 4, 255] {
        let mut bytes = fixture("ohlcv_1m_v3").bytes;
        bytes[3] = version;
        assert!(matches!(decode(&bytes), Err(DbnError::UnsupportedVersion { version: v }) if v == version));
    }
    assert!(matches!(decode(b""), Err(DbnError::Truncated { offset: 0, needed: 8, available: 0 })));
}

#[test]
fn a_record_shorter_than_its_layout_is_rejected_and_stops_the_decoder() {
    for (name, rtype, length) in [
        ("definition_v1", rtype::INSTRUMENT_DEF, 360),
        ("definition_v2", rtype::INSTRUMENT_DEF, 400),
        ("definition_v3", rtype::INSTRUMENT_DEF, 520),
    ] {
        let mut bytes = fixture(name).bytes;
        let start = boundaries(&bytes)[0];
        bytes[start] -= 1;
        let mut decoder = Decoder::new(bytes.as_slice()).unwrap();
        for _ in 0..2 {
            let err = decoder.next().unwrap_err();
            assert!(
                matches!(err, DbnError::RecordTooShort { offset, rtype: r, length: l, min_length } if offset as usize == start && r == rtype && l == length - 4 && min_length == length),
                "{name}: {err:?}"
            );
        }
    }
    // With ts_out the 8 bytes of `ts_out` count towards the length.
    let mut bytes = fixture("live_ts_out_v3").bytes;
    let start = boundaries(&bytes)[0];
    assert_eq!(bytes[start + 1], rtype::SYSTEM);
    bytes[start] -= 2;
    assert!(matches!(decode(&bytes), Err(DbnError::RecordTooShort { length: 320, min_length: 328, .. })));
    // A zero length cannot frame any record.
    let mut bytes = fixture("live_v1").bytes;
    let start = boundaries(&bytes)[0];
    bytes[start] = 0;
    assert!(matches!(decode(&bytes), Err(DbnError::RecordTooShort { length: 0, .. })));
}

#[test]
fn a_record_longer_than_its_layout_is_read_up_to_its_layout() {
    let (metadata, records) = decode(&fixture("statistics_v3").bytes).unwrap();
    let mut bytes = encode(&metadata, &records[..1]);
    let start = boundaries(&bytes)[0];
    bytes[start] += 2;
    bytes.extend_from_slice(&[0xAB; 8]);
    bytes.extend_from_slice(&encode(&metadata, &records[1..2])[start..]);
    let (_, decoded) = decode(&bytes).unwrap();
    assert_eq!(decoded, records[..2]);
}

#[test]
fn a_string_without_terminator_or_with_invalid_utf8_is_rejected() {
    let (metadata, records) = decode(&fixture("definition_v3").bytes).unwrap();
    let mut def = first_def(&records);
    def.raw_symbol = "MARKER".to_owned();
    let records = [
        Decoded {
            record: Record::InstrumentDef(Box::new(def)),
            ts_out: None,
        },
        records[1].clone(),
    ];
    let bytes = encode(&metadata, &records);
    let pos = find(&bytes, b"MARKER\0");

    let mut unterminated = bytes.clone();
    unterminated[pos..pos + SYMBOL_CSTR_LEN_V2].fill(b'X');
    let mut decoder = Decoder::new(unterminated.as_slice()).unwrap();
    let err = decoder.next().unwrap_err();
    assert!(
        matches!(err, DbnError::InvalidCString { field: "InstrumentDef::raw_symbol", offset, reason: "no NUL terminator" } if offset as usize == pos),
        "{err:?}"
    );
    // The bad record is consumed and decoding continues with the next one.
    assert_eq!(decoder.next().unwrap().as_ref(), Some(&records[1]));
    assert!(decoder.next().unwrap().is_none());

    let mut not_utf8 = bytes.clone();
    not_utf8[pos] = 0xFF;
    assert!(matches!(
        decode(&not_utf8),
        Err(DbnError::InvalidCString {
            field: "InstrumentDef::raw_symbol",
            reason: "not valid UTF-8",
            ..
        })
    ));

    let mut dataset = bytes;
    dataset[8..24].fill(b'X');
    assert!(matches!(
        decode(&dataset),
        Err(DbnError::InvalidCString {
            field: "Metadata::dataset",
            offset: 8,
            ..
        })
    ));
}

#[test]
fn metadata_that_does_not_fit_its_length_is_rejected() {
    // The symbol count follows the 100-byte fixed part and the 4-byte schema definition length.
    let mut bytes = fixture("ohlcv_1m_v2").bytes;
    bytes[112..116].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(decode(&bytes), Err(DbnError::InvalidMetadata { .. })));
    // The symbol width of versions 2 and 3 follows the dataset, schema, start, end, limit and stypes.
    let mut bytes = fixture("ohlcv_1m_v2").bytes;
    bytes[53..55].copy_from_slice(&0u16.to_le_bytes());
    assert!(matches!(decode(&bytes), Err(DbnError::InvalidMetadata { .. })));
    // One mapping more than the metadata holds: the count fits the remaining bytes, but the mappings
    // before it use them up.
    let mut bytes = fixture("ohlcv_1m_v2").bytes;
    let (metadata, _) = decode(&bytes).unwrap();
    let strings = metadata.symbols.len() + metadata.partial.len() + metadata.not_found.len();
    let pos = 112 + 3 * 4 + strings * metadata.symbol_cstr_len;
    assert_eq!(bytes[pos..pos + 4], (metadata.mappings.len() as u32).to_le_bytes());
    bytes[pos..pos + 4].copy_from_slice(&(metadata.mappings.len() as u32 + 1).to_le_bytes());
    assert!(matches!(decode(&bytes), Err(DbnError::InvalidMetadata { .. })));
    // A metadata length shorter than the fixed part.
    let mut bytes = fixture("ohlcv_1m_v1").bytes;
    bytes[4..8].copy_from_slice(&50u32.to_le_bytes());
    assert!(matches!(decode(&bytes), Err(DbnError::InvalidMetadata { .. })));
}

// ---------------------------------------------------------------------------------------------------
// Conversions between versions

#[test]
fn records_convert_between_versions() {
    // OHLCV bars have the same layout in every version.
    let (metadata, records) = decode(&fixture("ohlcv_1m_v1").bytes).unwrap();
    for version in 2..=3 {
        let converted = Metadata {
            version,
            symbol_cstr_len: SYMBOL_CSTR_LEN_V2,
            ..metadata.clone()
        };
        assert_eq!(decode(&encode(&converted, &records)).unwrap(), (converted, records.clone()));
    }
    // Version 3 definitions lack four fields of version 2, which read back as their undefined values.
    let (metadata, records) = decode(&fixture("definition_v2").bytes).unwrap();
    let (_, converted) = decode(&encode(&Metadata { version: 3, ..metadata }, &records)).unwrap();
    let undefined = InstrumentDef::default();
    for (original, converted) in records.iter().zip(&converted) {
        let (Record::InstrumentDef(original), Record::InstrumentDef(converted)) = (&original.record, &converted.record) else {
            panic!("definitions expected");
        };
        let expected = InstrumentDef {
            trading_reference_price: undefined.trading_reference_price,
            trading_reference_date: undefined.trading_reference_date,
            md_security_trading_status: undefined.md_security_trading_status,
            settl_price_type: undefined.settl_price_type,
            ..(**original).clone()
        };
        assert_eq!(**converted, expected);
    }
}

#[test]
fn ts_out_is_written_only_when_the_metadata_says_so() {
    let (metadata, records) = decode(&fixture("live_ts_out_v3").bytes).unwrap();
    assert!(metadata.ts_out && records.iter().all(|d| d.ts_out.is_some()));
    let without = Metadata { ts_out: false, ..metadata.clone() };
    let (_, decoded) = decode(&encode(&without, &records)).unwrap();
    let stripped: Vec<Decoded> = records.iter().map(|d| Decoded { ts_out: None, ..d.clone() }).collect();
    assert_eq!(decoded, stripped);
    // A missing ts_out is written as the undefined timestamp.
    let (_, decoded) = decode(&encode(&metadata, &stripped)).unwrap();
    assert!(decoded.iter().all(|d| d.ts_out == Some(u64::MAX)));
    let hd = Header {
        rtype: 0xA0,
        publisher_id: 1,
        instrument_id: 2,
        ts_event: 3,
    };
    let other = [Decoded {
        record: Record::Other { hd, body: vec![1, 2, 3, 4, 5] },
        ts_out: Some(9),
    }];
    let (_, decoded) = decode(&encode(&metadata, &other)).unwrap();
    assert_eq!(
        decoded[0].record,
        Record::Other {
            hd,
            body: vec![1, 2, 3, 4, 5, 0, 0, 0]
        },
        "the body is padded to 4 bytes"
    );
    assert_eq!(decoded[0].ts_out, Some(9));
}
