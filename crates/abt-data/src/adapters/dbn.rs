//! Decoder and encoder for DBN (Databento Binary Encoding), versions 1, 2 and 3.
//!
//! DBN is the binary format of Databento's market data files and live data streams. A DBN stream is a
//! metadata header followed by a sequence of records. This module reads and writes uncompressed DBN with
//! the standard library only; a Zstandard-compressed file (`.dbn.zst`) has to be decompressed first.
//!
//! # Wire format
//!
//! Every integer is little-endian.
//!
//! - The metadata starts with the three bytes `DBN`, a version byte (1, 2 or 3) and a `u32` holding the
//!   number of metadata bytes that follow. [`Metadata`] describes its fields.
//! - Every record starts with a 16-byte header: a length byte, the record type (`rtype`, one byte), the
//!   publisher ID (`u16`), the instrument ID (`u32`) and `ts_event` (`u64`). The length byte counts the
//!   whole record in units of 4 bytes, so a 56-byte record stores 14. The rtype selects the layout of the
//!   rest of the record.
//! - When [`Metadata::ts_out`] is set, as in live data requested with send timestamps, every record is
//!   followed by an 8-byte `ts_out` timestamp and the record's length byte includes those 8 bytes.
//!
//! Prices are fixed-point `i64` values in units of 1e-9, so `1_500_000_000` is 1.5; [`price`] converts one
//! to `f64`. Timestamps are `u64` nanoseconds since the UNIX epoch.
//!
//! # Records
//!
//! The decoder returns a typed [`Record`] for the record types below, in the layout of the stream's version,
//! and [`Record::Other`] with the raw body for any other rtype. Sizes are in bytes and exclude `ts_out`.
//!
//! | Variant | rtype | v1 | v2 | v3 |
//! |---|---|---|---|---|
//! | [`Record::Ohlcv`] | `0x11`, `0x20` to `0x24` | 56 | 56 | 56 |
//! | [`Record::InstrumentDef`] | `0x13` | 360 | 400 | 520 |
//! | [`Record::Statistics`] | `0x18` | 64 | 64 | 80 |
//! | [`Record::SymbolMapping`] | `0x16` | 80 | 176 | 176 |
//! | [`Record::Error`] | `0x15` | 80 | 320 | 320 |
//! | [`Record::System`] | `0x17` | 80 | 320 | 320 |
//!
//! A record longer than its layout is accepted and the bytes beyond the layout are ignored. A record
//! shorter than its layout is an error ([`DbnError::RecordTooShort`]).
//!
//! # Differences between versions
//!
//! - Symbol strings are 22 bytes wide in version 1 and 71 bytes wide in versions 2 and 3, in records and,
//!   as written by Databento, in the metadata.
//! - Version 3 instrument definitions add the leg fields of spreads, widen `raw_instrument_id` to 64 bits
//!   and `asset` to 11 bytes, and drop `trading_reference_price`, `trading_reference_date`,
//!   `md_security_trading_status` and `settl_price_type`. [`InstrumentDef`] holds the fields of all three
//!   versions; a field that the decoded version lacks holds its value in [`InstrumentDef::default`], which
//!   is DBN's undefined value for the field and the value the reference implementation gives the field
//!   when it upgrades a record to version 3.
//! - Version 3 statistics widen `quantity` from `i32` to `i64`. Decoding a version 1 or 2 record widens the
//!   value and maps the undefined `i32::MAX` to [`UNDEF_STAT_QUANTITY`], as the reference upgrade does.
//! - Version 1 symbol mappings have no `stype_in` and `stype_out`, version 1 errors have no `code` and
//!   `is_last`, and version 1 system messages have no `code`. These fields decode as `None`.
//!
//! # Undefined values
//!
//! Numeric record fields keep DBN's sentinels for undefined values: [`UNDEF_PRICE`] (`i64::MAX`) for
//! prices, [`UNDEF_TIMESTAMP`] (`u64::MAX`) for timestamps and, for most other fields, the maximum value of
//! the field's integer type. Fields typed `Option` map their sentinel to `None`:
//!
//! - the metadata `schema` (`u16::MAX`), `end` (`u64::MAX`; a stored 0 also reads as `None`, as in the
//!   reference decoder), `limit` (0) and `stype_in` (`u8::MAX`);
//! - the symbol mapping `stype_in` and `stype_out` (`u8::MAX`);
//! - the error and system `code` (`u8::MAX`, DBN's `Unset` code) and the error `is_last` (`u8::MAX`; 0
//!   reads as `Some(false)` and any other value as `Some(true)`).
//!
//! # Encoding
//!
//! [`encode`] writes the layouts of `metadata.version`. For a stream written by the reference
//! implementation, encoding the output of [`decode`] reproduces the input byte for byte. The decoder does
//! not keep the following, and [`encode`] writes them as the reference encoder does:
//!
//! - reserved and padding bytes, written as zero;
//! - bytes after the NUL terminator of a fixed-width string, written as zero;
//! - bytes of a record beyond its layout, which are dropped;
//! - the version 1 metadata `record_count`, written as `u64::MAX`;
//! - the schema definition in the metadata (never present in versions 1 to 3), written as empty;
//! - bytes between the symbol mappings and the end of the metadata, written as the zero padding with which
//!   version 3 aligns the metadata length to 8 bytes (versions 1 and 2 have none);
//! - a metadata `ts_out` byte other than 0 or 1 and an error `is_last` byte other than 0, 1 or `u8::MAX`,
//!   both written as 1.

use std::fmt;
use std::io::{self, Read};

/// The scale of fixed-point prices: a price field holds the price multiplied by this value.
pub const FIXED_PRICE_SCALE: i64 = 1_000_000_000;
/// The undefined value of price fields.
pub const UNDEF_PRICE: i64 = i64::MAX;
/// The undefined value of timestamp fields.
pub const UNDEF_TIMESTAMP: u64 = u64::MAX;
/// The undefined value of the statistics `quantity` field; versions 1 and 2 store `i32::MAX` on the wire.
pub const UNDEF_STAT_QUANTITY: i64 = i64::MAX;
/// The number of bytes in one unit of the record length byte.
pub const LENGTH_MULTIPLIER: usize = 4;
/// The size in bytes of the record header.
pub const HEADER_LEN: usize = 16;
/// The size in bytes of the `ts_out` timestamp that follows each record when [`Metadata::ts_out`] is set.
pub const TS_OUT_LEN: usize = 8;
/// The width in bytes of symbol strings in version 1, including the NUL terminator.
pub const SYMBOL_CSTR_LEN_V1: usize = 22;
/// The width in bytes of symbol strings in version 2 and 3 records, including the NUL terminator.
pub const SYMBOL_CSTR_LEN_V2: usize = 71;

/// Record type (`rtype`) values of the typed [`Record`] variants.
pub mod rtype {
    /// An OHLCV bar of an unspecified interval, used by early DBN data.
    pub const OHLCV_DEPRECATED: u8 = 0x11;
    /// An OHLCV bar of one second.
    pub const OHLCV_1S: u8 = 0x20;
    /// An OHLCV bar of one minute.
    pub const OHLCV_1M: u8 = 0x21;
    /// An OHLCV bar of one hour.
    pub const OHLCV_1H: u8 = 0x22;
    /// An OHLCV bar of one UTC day.
    pub const OHLCV_1D: u8 = 0x23;
    /// An OHLCV bar of one day, delimited by the end of the trading session instead of the UTC date.
    pub const OHLCV_EOD: u8 = 0x24;
    /// An instrument definition.
    pub const INSTRUMENT_DEF: u8 = 0x13;
    /// An error message from the live gateway.
    pub const ERROR: u8 = 0x15;
    /// A symbol mapping from the live gateway.
    pub const SYMBOL_MAPPING: u8 = 0x16;
    /// A non-error message from the live gateway, such as a heartbeat.
    pub const SYSTEM: u8 = 0x17;
    /// A statistic published by the venue.
    pub const STATISTICS: u8 = 0x18;
}

/// Converts a fixed-point price to `f64`: `None` for [`UNDEF_PRICE`], otherwise `x / 1e9`.
pub fn price(x: i64) -> Option<f64> {
    if x == UNDEF_PRICE {
        None
    } else {
        Some(x as f64 / FIXED_PRICE_SCALE as f64)
    }
}

/// The metadata at the start of a DBN stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    /// The DBN version of the stream: 1, 2 or 3.
    pub version: u8,
    /// The dataset code, such as `GLBX.MDP3`; at most 15 bytes.
    pub dataset: String,
    /// The schema of the records as a DBN `Schema` value; `None` (stored as `u16::MAX`) when the stream can
    /// mix schemas, as live data does.
    pub schema: Option<u16>,
    /// The start of the requested time range, in nanoseconds since the UNIX epoch.
    pub start: u64,
    /// The end of the requested time range, in nanoseconds since the UNIX epoch; `None` when undefined
    /// (stored as `u64::MAX`). A stored 0 also reads as `None`, so `Some(0)` does not survive a round trip.
    pub end: Option<u64>,
    /// The maximum number of records requested; `None` when there is no limit (stored as 0, so `Some(0)`
    /// reads back as `None`).
    pub limit: Option<u64>,
    /// The symbology type of the requested symbols as a DBN `SType` value; `None` (stored as `u8::MAX`) when
    /// the stream mixes symbology types, as live data does.
    pub stype_in: Option<u8>,
    /// The symbology type of the resolved symbols as a DBN `SType` value.
    pub stype_out: u8,
    /// Whether each record is followed by an 8-byte `ts_out` send timestamp.
    pub ts_out: bool,
    /// The width in bytes of the symbol strings of the metadata, including the NUL terminator. Version 1
    /// has no such field and always uses 22; versions 2 and 3 store it (Databento writes 71).
    pub symbol_cstr_len: usize,
    /// The symbols of the original request.
    pub symbols: Vec<String>,
    /// The requested symbols that did not resolve for at least one day of the time range.
    pub partial: Vec<String>,
    /// The requested symbols that did not resolve for any day of the time range.
    pub not_found: Vec<String>,
    /// The resolutions of the requested symbols over the time range.
    pub mappings: Vec<SymbolMapping>,
}

/// The resolutions of one requested symbol over the requested time range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolMapping {
    /// The requested symbol, in the `stype_in` symbology.
    pub raw_symbol: String,
    /// The intervals over which `raw_symbol` resolved to a symbol in the `stype_out` symbology.
    pub intervals: Vec<MappingInterval>,
}

/// One interval of a [`SymbolMapping`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappingInterval {
    /// The first UTC date of the interval (inclusive) as the integer YYYYMMDD.
    pub start_date: u32,
    /// The UTC date after the end of the interval (exclusive) as the integer YYYYMMDD.
    pub end_date: u32,
    /// The resolved symbol in the `stype_out` symbology; for instrument IDs, the ID in decimal.
    pub symbol: String,
}

/// The header common to all records, without the length byte, which follows from the record's layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// The record type, which selects the layout of the record.
    pub rtype: u8,
    /// The Databento publisher ID, which identifies the dataset and the venue.
    pub publisher_id: u16,
    /// The numeric instrument ID.
    pub instrument_id: u32,
    /// The time the matching engine received the event, in nanoseconds since the UNIX epoch.
    pub ts_event: u64,
}

/// An instrument definition, decoded from the version 1, 2 or 3 layout.
///
/// The struct holds the fields of all three versions. A field that the decoded version lacks holds its
/// value in [`InstrumentDef::default`]; the field documentation names the versions that store each field.
/// Unless stated otherwise a field is present in all versions. Single-character fields hold the ASCII code
/// of the character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstrumentDef {
    /// The record header.
    pub hd: Header,
    /// The time the capture server received the definition, in nanoseconds since the UNIX epoch.
    pub ts_recv: u64,
    /// The minimum price tick, as a fixed-point price.
    pub min_price_increment: i64,
    /// The multiplier that converts the venue's display price to the conventional price, as a fixed-point
    /// value.
    pub display_factor: i64,
    /// The last eligible trade time, in nanoseconds since the UNIX epoch.
    pub expiration: u64,
    /// The time of instrument activation, in nanoseconds since the UNIX epoch.
    pub activation: u64,
    /// The allowed high limit price for the trading day, as a fixed-point price.
    pub high_limit_price: i64,
    /// The allowed low limit price for the trading day, as a fixed-point price.
    pub low_limit_price: i64,
    /// The differential value for price banding, as a fixed-point price.
    pub max_price_variation: i64,
    /// The trading session settlement price on `trading_reference_date`, as a fixed-point price. Versions 1
    /// and 2 only.
    pub trading_reference_price: i64,
    /// The contract size in `unit_of_measure` units, as a fixed-point value.
    pub unit_of_measure_qty: i64,
    /// The minimum price increment amount disseminated by the venue, as a fixed-point price.
    pub min_price_increment_amount: i64,
    /// The value used for price calculation in spread and leg pricing, as a fixed-point value.
    pub price_ratio: i64,
    /// The strike price of an option, as a fixed-point price.
    pub strike_price: i64,
    /// The instrument ID assigned by the publisher. 32 bits wide in versions 1 and 2.
    pub raw_instrument_id: u64,
    /// The tied price of the leg, as a fixed-point price. Version 3 only.
    pub leg_price: i64,
    /// The delta of the leg, as a fixed-point value. Version 3 only.
    pub leg_delta: i64,
    /// The venue-specific instrument attributes, a bit field.
    pub inst_attrib_value: i32,
    /// The instrument ID of the first underlying instrument.
    pub underlying_id: u32,
    /// The implied book depth of the price level data feed.
    pub market_depth_implied: i32,
    /// The outright book depth of the price level data feed.
    pub market_depth: i32,
    /// The market segment of the instrument.
    pub market_segment_id: u32,
    /// The maximum trading volume of the instrument.
    pub max_trade_vol: u32,
    /// The minimum order entry quantity of the instrument.
    pub min_lot_size: i32,
    /// The minimum quantity of a block trade of the instrument.
    pub min_lot_size_block: i32,
    /// The minimum quantity of a round lot of the instrument.
    pub min_lot_size_round_lot: i32,
    /// The minimum trading volume of the instrument.
    pub min_trade_vol: u32,
    /// The number of deliverables per instrument, such as peak days.
    pub contract_multiplier: i32,
    /// The quantity by which a contract decays daily after `decay_start_date`.
    pub decay_quantity: i32,
    /// The fixed contract value assigned to each instrument.
    pub original_contract_size: i32,
    /// The instrument ID of the leg instrument. Version 3 only.
    pub leg_instrument_id: u32,
    /// The numerator of the price ratio of the leg within the spread. Version 3 only.
    pub leg_ratio_price_numerator: i32,
    /// The denominator of the price ratio of the leg within the spread. Version 3 only.
    pub leg_ratio_price_denominator: i32,
    /// The numerator of the quantity ratio of the leg within the spread. Version 3 only.
    pub leg_ratio_qty_numerator: i32,
    /// The denominator of the quantity ratio of the leg within the spread. Version 3 only.
    pub leg_ratio_qty_denominator: i32,
    /// The instrument ID of the leg instrument's underlying instrument. Version 3 only.
    pub leg_underlying_id: u32,
    /// The trading session date of `trading_reference_price`, in days since the UNIX epoch. Versions 1 and 2
    /// only.
    pub trading_reference_date: u16,
    /// The channel ID assigned at the venue.
    pub appl_id: i16,
    /// The calendar year in the instrument symbol.
    pub maturity_year: u16,
    /// The date on which a contract starts to decay.
    pub decay_start_date: u16,
    /// The channel ID assigned by Databento, counting up from zero.
    pub channel_id: u16,
    /// The number of legs of the strategy or spread; 0 for outrights. Version 3 only.
    pub leg_count: u16,
    /// The zero-based index of the leg. Version 3 only.
    pub leg_index: u16,
    /// The currency of the price fields (3 bytes at most).
    pub currency: String,
    /// The currency of settlement, if it differs from `currency` (3 bytes at most).
    pub settl_currency: String,
    /// The strategy type of the spread (5 bytes at most).
    pub secsubtype: String,
    /// The raw symbol assigned by the publisher (21 bytes at most in version 1, 70 in versions 2 and 3).
    pub raw_symbol: String,
    /// The security group code of the instrument (20 bytes at most).
    pub group: String,
    /// The exchange of the instrument (4 bytes at most).
    pub exchange: String,
    /// The underlying asset code, or product code (6 bytes at most in versions 1 and 2, 10 in version 3).
    pub asset: String,
    /// The ISO 10962 classification (CFI) code of the instrument (6 bytes at most).
    pub cfi: String,
    /// The security type of the instrument, such as `FUT` (6 bytes at most).
    pub security_type: String,
    /// The unit of measure of the contract size, such as `USD` or `LBS` (30 bytes at most).
    pub unit_of_measure: String,
    /// The symbol of the first underlying instrument (20 bytes at most).
    pub underlying: String,
    /// The currency of `strike_price` (3 bytes at most).
    pub strike_price_currency: String,
    /// The raw symbol of the leg instrument (70 bytes at most). Version 3 only.
    pub leg_raw_symbol: String,
    /// The class of the instrument, such as `F` for a future or `C` for a call option.
    pub instrument_class: u8,
    /// The matching algorithm of the instrument, such as `F` for FIFO.
    pub match_algorithm: u8,
    /// The trading status of the instrument. Versions 1 and 2 only.
    pub md_security_trading_status: u8,
    /// The price denominator of the main fraction.
    pub main_fraction: u8,
    /// The number of digits to the right of the tick mark in displayed fractional prices.
    pub price_display_format: u8,
    /// The type indicators of the settlement price, a bit field. Versions 1 and 2 only.
    pub settl_price_type: u8,
    /// The price denominator of the sub fraction.
    pub sub_fraction: u8,
    /// The product complex of the instrument.
    pub underlying_product: u8,
    /// Whether the definition is added (`A`), modified (`M`) or deleted (`D`).
    pub security_update_action: u8,
    /// The calendar month in the instrument symbol.
    pub maturity_month: u8,
    /// The calendar day in the instrument symbol, or 0.
    pub maturity_day: u8,
    /// The calendar week in the instrument symbol, or 0.
    pub maturity_week: u8,
    /// Whether the instrument is user-defined: `Y` or `N`.
    pub user_defined_instrument: u8,
    /// The unit of `contract_multiplier`: 1 for hours, 2 for days.
    pub contract_multiplier_unit: i8,
    /// The schedule for delivering electricity.
    pub flow_schedule_type: i8,
    /// The tick rule of the spread.
    pub tick_rule: u8,
    /// The class of the leg instrument. Version 3 only.
    pub leg_instrument_class: u8,
    /// The side taken for the leg when buying the spread. Version 3 only.
    pub leg_side: u8,
}

impl Default for InstrumentDef {
    /// Returns a definition whose fields hold DBN's undefined values, the defaults of the reference
    /// implementation: the price fields [`UNDEF_PRICE`], the time fields [`UNDEF_TIMESTAMP`], the strings
    /// empty, `raw_instrument_id`, `underlying_id`, `instrument_class` and the numeric leg fields 0,
    /// `match_algorithm` `' '`, `security_update_action` `'A'`, `user_defined_instrument` and `leg_side`
    /// `'N'`, and the other integers the maximum of their type. `hd.rtype` is
    /// [`rtype::INSTRUMENT_DEF`], `hd.ts_event` [`UNDEF_TIMESTAMP`] and the IDs 0.
    fn default() -> Self {
        Self {
            hd: Header {
                rtype: rtype::INSTRUMENT_DEF,
                publisher_id: 0,
                instrument_id: 0,
                ts_event: UNDEF_TIMESTAMP,
            },
            ts_recv: UNDEF_TIMESTAMP,
            min_price_increment: UNDEF_PRICE,
            display_factor: UNDEF_PRICE,
            expiration: UNDEF_TIMESTAMP,
            activation: UNDEF_TIMESTAMP,
            high_limit_price: UNDEF_PRICE,
            low_limit_price: UNDEF_PRICE,
            max_price_variation: UNDEF_PRICE,
            trading_reference_price: UNDEF_PRICE,
            unit_of_measure_qty: UNDEF_PRICE,
            min_price_increment_amount: UNDEF_PRICE,
            price_ratio: UNDEF_PRICE,
            strike_price: UNDEF_PRICE,
            raw_instrument_id: 0,
            leg_price: UNDEF_PRICE,
            leg_delta: UNDEF_PRICE,
            inst_attrib_value: i32::MAX,
            underlying_id: 0,
            market_depth_implied: i32::MAX,
            market_depth: i32::MAX,
            market_segment_id: u32::MAX,
            max_trade_vol: u32::MAX,
            min_lot_size: i32::MAX,
            min_lot_size_block: i32::MAX,
            min_lot_size_round_lot: i32::MAX,
            min_trade_vol: u32::MAX,
            contract_multiplier: i32::MAX,
            decay_quantity: i32::MAX,
            original_contract_size: i32::MAX,
            leg_instrument_id: 0,
            leg_ratio_price_numerator: 0,
            leg_ratio_price_denominator: 0,
            leg_ratio_qty_numerator: 0,
            leg_ratio_qty_denominator: 0,
            leg_underlying_id: 0,
            trading_reference_date: u16::MAX,
            appl_id: i16::MAX,
            maturity_year: u16::MAX,
            decay_start_date: u16::MAX,
            channel_id: u16::MAX,
            leg_count: 0,
            leg_index: 0,
            currency: String::new(),
            settl_currency: String::new(),
            secsubtype: String::new(),
            raw_symbol: String::new(),
            group: String::new(),
            exchange: String::new(),
            asset: String::new(),
            cfi: String::new(),
            security_type: String::new(),
            unit_of_measure: String::new(),
            underlying: String::new(),
            strike_price_currency: String::new(),
            leg_raw_symbol: String::new(),
            instrument_class: 0,
            match_algorithm: b' ',
            md_security_trading_status: u8::MAX,
            main_fraction: u8::MAX,
            price_display_format: u8::MAX,
            settl_price_type: u8::MAX,
            sub_fraction: u8::MAX,
            underlying_product: u8::MAX,
            security_update_action: b'A',
            maturity_month: u8::MAX,
            maturity_day: u8::MAX,
            maturity_week: u8::MAX,
            user_defined_instrument: b'N',
            contract_multiplier_unit: i8::MAX,
            flow_schedule_type: i8::MAX,
            tick_rule: u8::MAX,
            leg_instrument_class: 0,
            leg_side: b'N',
        }
    }
}

/// A DBN record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// An OHLCV bar: the open, high, low and close prices and the volume of an interval. `hd.rtype` gives
    /// the interval (see [`rtype`]).
    Ohlcv {
        /// The record header.
        hd: Header,
        /// The open price, as a fixed-point price.
        open: i64,
        /// The high price, as a fixed-point price.
        high: i64,
        /// The low price, as a fixed-point price.
        low: i64,
        /// The close price, as a fixed-point price.
        close: i64,
        /// The total volume traded in the interval.
        volume: u64,
    },
    /// An instrument definition, boxed because it is far larger than the other variants.
    InstrumentDef(Box<InstrumentDef>),
    /// A statistic published by the venue, such as a settlement price or the open interest.
    Statistics {
        /// The record header.
        hd: Header,
        /// The time the capture server received the statistic, in nanoseconds since the UNIX epoch.
        ts_recv: u64,
        /// The reference time of the statistic, in nanoseconds since the UNIX epoch, or [`UNDEF_TIMESTAMP`].
        ts_ref: u64,
        /// The value of a price statistic, as a fixed-point price, or [`UNDEF_PRICE`].
        price: i64,
        /// The value of a quantity statistic, or [`UNDEF_STAT_QUANTITY`]. Versions 1 and 2 store an `i32`.
        quantity: i64,
        /// The message sequence number assigned at the venue.
        sequence: u32,
        /// The number of nanoseconds between the matching engine sending the message and `ts_recv`.
        ts_in_delta: i32,
        /// The type of the statistic as a DBN `StatType` value, such as 3 for a settlement price or 9 for
        /// the open interest.
        stat_type: u16,
        /// The channel ID assigned by Databento, counting up from zero.
        channel_id: u16,
        /// Whether the statistic is new (1) or deleted (2).
        update_action: u8,
        /// Additional flags of some statistic types, a bit field.
        stat_flags: u8,
    },
    /// A symbol mapping from the live gateway, which maps a subscribed symbol to the symbol of an instrument.
    SymbolMapping {
        /// The record header; `hd.instrument_id` is the instrument the symbols map to.
        hd: Header,
        /// The symbology type of `stype_in_symbol` as a DBN `SType` value; `None` in version 1 or when
        /// undefined.
        stype_in: Option<u8>,
        /// The subscribed symbol (21 bytes at most in version 1, 70 in versions 2 and 3).
        stype_in_symbol: String,
        /// The symbology type of `stype_out_symbol` as a DBN `SType` value; `None` in version 1 or when
        /// undefined.
        stype_out: Option<u8>,
        /// The resolved symbol (21 bytes at most in version 1, 70 in versions 2 and 3).
        stype_out_symbol: String,
        /// The start of the mapping interval, in nanoseconds since the UNIX epoch.
        start_ts: u64,
        /// The end of the mapping interval, in nanoseconds since the UNIX epoch.
        end_ts: u64,
    },
    /// An error message from the live gateway.
    Error {
        /// The record header.
        hd: Header,
        /// The error message (63 bytes at most in version 1, 301 in versions 2 and 3).
        err: String,
        /// The error code as a DBN `ErrorCode` value; `None` in version 1 or when unset.
        code: Option<u8>,
        /// Whether this is the last of a group of errors sent together; `None` in version 1 or when unset.
        is_last: Option<bool>,
    },
    /// A non-error message from the live gateway, such as a heartbeat or a subscription acknowledgement.
    System {
        /// The record header.
        hd: Header,
        /// The message (63 bytes at most in version 1, 302 in versions 2 and 3).
        msg: String,
        /// The message code as a DBN `SystemCode` value; `None` in version 1 or when unset.
        code: Option<u8>,
    },
    /// A record of a type this module does not decode, kept as raw bytes.
    Other {
        /// The record header.
        hd: Header,
        /// The bytes of the record after the 16-byte header, excluding `ts_out`.
        body: Vec<u8>,
    },
}

impl Record {
    /// Returns the header of the record.
    pub fn header(&self) -> &Header {
        match self {
            Record::Ohlcv { hd, .. } | Record::Statistics { hd, .. } | Record::SymbolMapping { hd, .. } | Record::Error { hd, .. } | Record::System { hd, .. } | Record::Other { hd, .. } => hd,
            Record::InstrumentDef(def) => &def.hd,
        }
    }
}

/// A record and the `ts_out` that followed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// The record.
    pub record: Record,
    /// The time the live gateway sent the record, in nanoseconds since the UNIX epoch; `None` when
    /// [`Metadata::ts_out`] is not set.
    pub ts_out: Option<u64>,
}

/// An error from decoding DBN.
#[derive(Debug)]
pub enum DbnError {
    /// The input ended inside the metadata or inside a record.
    Truncated {
        /// The byte offset in the input of the start of the incomplete section: 0 for the 8-byte metadata
        /// prefix, 8 for the rest of the metadata, or the start of the record.
        offset: u64,
        /// The number of bytes the section needs.
        needed: usize,
        /// The number of bytes of the section the input holds.
        available: usize,
    },
    /// The input does not start with the bytes `DBN`.
    BadMagic {
        /// The first three bytes of the input.
        found: [u8; 3],
    },
    /// The version byte is not 1, 2 or 3.
    UnsupportedVersion {
        /// The version byte of the input.
        version: u8,
    },
    /// The metadata is shorter than its fixed part, holds counts or a schema definition that do not fit its
    /// length, or has a symbol width of 0.
    InvalidMetadata {
        /// A description of the problem.
        reason: String,
    },
    /// A record's length is shorter than the layout of its record type in the stream's version.
    RecordTooShort {
        /// The byte offset of the record in the input.
        offset: u64,
        /// The record type.
        rtype: u8,
        /// The length of the record in bytes, from its length byte.
        length: usize,
        /// The minimum length in bytes for the record type, including `ts_out` when present.
        min_length: usize,
    },
    /// A fixed-width string field has no NUL terminator or is not valid UTF-8.
    InvalidCString {
        /// The name of the field.
        field: &'static str,
        /// The byte offset of the field in the input.
        offset: u64,
        /// The problem with the field.
        reason: &'static str,
    },
    /// The reader returned an I/O error.
    Io(io::Error),
}

impl fmt::Display for DbnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbnError::Truncated { offset, needed, available } => {
                write!(f, "truncated DBN input: the {needed}-byte section at byte {offset} ends after {available} bytes")
            }
            DbnError::BadMagic { found } => write!(f, "not DBN input: expected the bytes \"DBN\", found {found:?}"),
            DbnError::UnsupportedVersion { version } => write!(f, "unsupported DBN version {version}: versions 1 to 3 are supported"),
            DbnError::InvalidMetadata { reason } => write!(f, "invalid DBN metadata: {reason}"),
            DbnError::RecordTooShort { offset, rtype, length, min_length } => {
                write!(
                    f,
                    "DBN record at byte {offset} with rtype {rtype:#04x} is {length} bytes long, shorter than the {min_length} bytes of its layout"
                )
            }
            DbnError::InvalidCString { field, offset, reason } => write!(f, "invalid DBN string field {field} at byte {offset}: {reason}"),
            DbnError::Io(err) => write!(f, "I/O error while reading DBN: {err}"),
        }
    }
}

impl std::error::Error for DbnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbnError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for DbnError {
    fn from(err: io::Error) -> Self {
        DbnError::Io(err)
    }
}

/// A streaming DBN decoder over a [`Read`] source.
///
/// [`Decoder::new`] reads the metadata and each call to [`Decoder::next`] reads one record. The decoder
/// requests only the bytes it still needs and accepts any number of bytes per read, so it works on sources
/// that deliver data in small pieces, such as a TCP stream. Reads interrupted with
/// [`io::ErrorKind::Interrupted`] are retried.
///
/// After an error from [`Decoder::next`]:
/// - [`DbnError::Io`] and [`DbnError::Truncated`] keep the bytes of the partial record, and the next call
///   continues reading it, so a non-blocking source can be polled again;
/// - [`DbnError::InvalidCString`] consumes the record, and the next call decodes the following record;
/// - [`DbnError::RecordTooShort`] leaves the decoder on the record, since its length cannot be trusted to
///   frame the next one, and every later call returns the same error.
pub struct Decoder<R: Read> {
    reader: R,
    metadata: Metadata,
    /// The bytes read so far of the record at `offset`.
    buf: Vec<u8>,
    /// The byte offset in the input of the record being read.
    offset: u64,
}

impl<R: Read> Decoder<R> {
    /// Reads and parses the metadata from `reader` and returns a decoder positioned at the first record.
    ///
    /// # Errors
    ///
    /// Returns [`DbnError::BadMagic`] or [`DbnError::UnsupportedVersion`] for input that is not DBN of
    /// versions 1 to 3, [`DbnError::Truncated`] if the input ends inside the metadata,
    /// [`DbnError::InvalidMetadata`] or [`DbnError::InvalidCString`] for malformed metadata, and
    /// [`DbnError::Io`] if the reader fails. A failed call consumes an unspecified number of bytes.
    pub fn new(mut reader: R) -> Result<Self, DbnError> {
        let mut prefix = [0u8; METADATA_PREFIX_LEN];
        let available = read_full(&mut reader, &mut prefix)?;
        if available >= 3 && &prefix[..3] != b"DBN" {
            return Err(DbnError::BadMagic {
                found: [prefix[0], prefix[1], prefix[2]],
            });
        }
        if available >= 4 && !(1..=3).contains(&prefix[3]) {
            return Err(DbnError::UnsupportedVersion { version: prefix[3] });
        }
        if available < METADATA_PREFIX_LEN {
            return Err(DbnError::Truncated {
                offset: 0,
                needed: METADATA_PREFIX_LEN,
                available,
            });
        }
        let version = prefix[3];
        let length = u32::from_le_bytes([prefix[4], prefix[5], prefix[6], prefix[7]]) as usize;
        let mut body = Vec::new();
        reader.by_ref().take(length as u64).read_to_end(&mut body)?;
        if body.len() < length {
            return Err(DbnError::Truncated {
                offset: METADATA_PREFIX_LEN as u64,
                needed: length,
                available: body.len(),
            });
        }
        let metadata = parse_metadata(version, &body)?;
        Ok(Self {
            reader,
            metadata,
            buf: Vec::with_capacity(MAX_LENGTH),
            offset: (METADATA_PREFIX_LEN + length) as u64,
        })
    }

    /// Returns the metadata of the stream.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Reads and decodes the next record, or returns `Ok(None)` when the input ends at a record boundary.
    ///
    /// # Errors
    ///
    /// Returns [`DbnError::Truncated`] if the input ends inside a record, [`DbnError::RecordTooShort`] if a
    /// record's length is shorter than its layout, [`DbnError::InvalidCString`] if a string field is not a
    /// NUL-terminated UTF-8 string, and [`DbnError::Io`] if the reader fails. The type documentation
    /// describes the state of the decoder after each error.
    #[allow(clippy::should_implement_trait)] // `Iterator::next` cannot tell a clean end from an error
    pub fn next(&mut self) -> Result<Option<Decoded>, DbnError> {
        if !self.fill(2)? {
            if self.buf.is_empty() {
                return Ok(None);
            }
            let needed = (usize::from(self.buf[0]) * LENGTH_MULTIPLIER).max(2);
            return Err(DbnError::Truncated {
                offset: self.offset,
                needed,
                available: self.buf.len(),
            });
        }
        let length = usize::from(self.buf[0]) * LENGTH_MULTIPLIER;
        let rtype = self.buf[1];
        let ts_out_len = if self.metadata.ts_out { TS_OUT_LEN } else { 0 };
        let min_length = layout_len(self.metadata.version, rtype) + ts_out_len;
        if length < min_length {
            return Err(DbnError::RecordTooShort {
                offset: self.offset,
                rtype,
                length,
                min_length,
            });
        }
        if !self.fill(length)? {
            return Err(DbnError::Truncated {
                offset: self.offset,
                needed: length,
                available: self.buf.len(),
            });
        }
        let (body, ts_out) = self.buf.split_at(length - ts_out_len);
        let ts_out = if ts_out_len == 0 { None } else { Some(Cursor::new(ts_out, 0).num::<u64>()) };
        let record = decode_record(self.metadata.version, body, self.offset);
        self.offset += length as u64;
        self.buf.clear();
        Ok(Some(Decoded { record: record?, ts_out }))
    }

    /// Reads from the source until `buf` holds `n` bytes. Returns `Ok(false)` if the source ends first.
    fn fill(&mut self, n: usize) -> Result<bool, DbnError> {
        while self.buf.len() < n {
            let start = self.buf.len();
            self.buf.resize(n, 0);
            match self.reader.read(&mut self.buf[start..]) {
                Ok(0) => {
                    self.buf.truncate(start);
                    return Ok(false);
                }
                Ok(read) => self.buf.truncate(start + read.min(n - start)),
                Err(err) => {
                    self.buf.truncate(start);
                    if err.kind() != io::ErrorKind::Interrupted {
                        return Err(DbnError::Io(err));
                    }
                }
            }
        }
        Ok(true)
    }
}

/// Decodes a complete DBN stream held in memory.
///
/// # Errors
///
/// Returns the first error that [`Decoder::new`] or [`Decoder::next`] returns for `bytes`.
pub fn decode(bytes: &[u8]) -> Result<(Metadata, Vec<Decoded>), DbnError> {
    let mut decoder = Decoder::new(bytes)?;
    let mut records = Vec::new();
    while let Some(record) = decoder.next()? {
        records.push(record);
    }
    Ok((decoder.metadata, records))
}

/// Encodes `metadata` and `records` as DBN in the layouts of `metadata.version`.
///
/// When `metadata.ts_out` is set, each record is followed by its `ts_out`, or by [`UNDEF_TIMESTAMP`] when
/// that is `None`; otherwise `ts_out` is ignored. The length byte of each record follows from its layout.
/// `hd.rtype` is written as given and has to be a record type of the record's variant, and a
/// [`Record::Other`] has to carry a record type that the decoder does not decode, or the output does not
/// decode to the same records.
///
/// Values the target version cannot hold are written as follows: a string longer than its field is cut at
/// the last character boundary that leaves room for the NUL terminator; in versions 1 and 2 a
/// `raw_instrument_id` above `u32::MAX` is written as 0 and a statistics `quantity` outside the `i32` range
/// as `i32::MAX` (undefined); the body of a [`Record::Other`] is padded with zero bytes to a multiple of 4.
/// For version 1, `metadata.symbol_cstr_len` is ignored and 22 is used.
///
/// # Panics
///
/// Panics if `metadata.version` is not 1, 2 or 3, if `metadata.symbol_cstr_len` is 0 or above `u16::MAX`
/// in versions 2 and 3, or if a [`Record::Other`] makes a record longer than the 1020 bytes a length byte
/// can describe.
pub fn encode(metadata: &Metadata, records: &[Decoded]) -> Vec<u8> {
    let version = metadata.version;
    assert!((1..=3).contains(&version), "DBN version {version} cannot be encoded: versions 1 to 3 are supported");
    let mut out = Vec::new();
    encode_metadata(metadata, &mut out);
    for decoded in records {
        let ts_out = if metadata.ts_out { Some(decoded.ts_out.unwrap_or(UNDEF_TIMESTAMP)) } else { None };
        encode_record(version, &decoded.record, ts_out, &mut out);
    }
    out
}

/// The length of the metadata prefix: the magic bytes, the version and the length.
const METADATA_PREFIX_LEN: usize = 8;
/// The length of the fixed-layout part of the metadata after the prefix, up to the schema definition length.
const METADATA_FIXED_LEN: usize = 100;
/// The width of the metadata dataset string.
const DATASET_CSTR_LEN: usize = 16;
/// The number of reserved bytes in the fixed part of version 1 metadata.
const METADATA_RESERVED_LEN_V1: usize = 47;
/// The number of reserved bytes in the fixed part of version 2 and 3 metadata.
const METADATA_RESERVED_LEN_V2: usize = 53;
/// The largest record length a length byte can describe.
const MAX_LENGTH: usize = u8::MAX as usize * LENGTH_MULTIPLIER;
/// The length of OHLCV records in all versions.
const OHLCV_LEN: usize = 56;

/// Returns the minimum length of a record of `rtype` in `version`, excluding `ts_out`.
fn layout_len(version: u8, rtype: u8) -> usize {
    match rtype {
        rtype::OHLCV_DEPRECATED | rtype::OHLCV_1S..=rtype::OHLCV_EOD => OHLCV_LEN,
        rtype::INSTRUMENT_DEF => match version {
            1 => 360,
            2 => 400,
            _ => 520,
        },
        rtype::STATISTICS if version < 3 => 64,
        rtype::STATISTICS => 80,
        rtype::SYMBOL_MAPPING if version == 1 => 80,
        rtype::SYMBOL_MAPPING => 176,
        rtype::ERROR | rtype::SYSTEM if version == 1 => 80,
        rtype::ERROR | rtype::SYSTEM => 320,
        _ => HEADER_LEN,
    }
}

/// Reads into `buf` until it is full or the source ends, retrying interrupted reads. Returns the number of
/// bytes read.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize, io::Error> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read.min(buf.len() - filled),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// A little-endian integer type of the wire format.
trait Wire: Copy {
    /// The size of the type in bytes.
    const SIZE: usize;
    /// Reads the value from `bytes`, treating missing bytes as zero.
    fn from_le(bytes: &[u8]) -> Self;
    /// Appends the value to `out`.
    fn put(self, out: &mut Vec<u8>);
}

macro_rules! impl_wire {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            const SIZE: usize = std::mem::size_of::<$t>();
            fn from_le(bytes: &[u8]) -> Self {
                let mut raw = [0u8; std::mem::size_of::<$t>()];
                let n = raw.len().min(bytes.len());
                raw[..n].copy_from_slice(&bytes[..n]);
                <$t>::from_le_bytes(raw)
            }
            fn put(self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
        }
    )*};
}

impl_wire!(u8, i8, u16, i16, u32, i32, u64, i64);

/// A read position in a byte slice. Reads past the end return the bytes that remain, which callers rule
/// out by checking lengths first.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
    /// The byte offset of `buf` in the input, for error messages.
    base: u64,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8], base: u64) -> Self {
        Self { buf, pos: 0, base }
    }

    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Returns the next `n` bytes, or the rest of the buffer if fewer remain.
    fn bytes(&mut self, n: usize) -> &'a [u8] {
        let end = self.pos.saturating_add(n).min(self.buf.len());
        let bytes = &self.buf[self.pos..end];
        self.pos = end;
        bytes
    }

    fn num<T: Wire>(&mut self) -> T {
        T::from_le(self.bytes(T::SIZE))
    }

    fn skip(&mut self, n: usize) {
        self.bytes(n);
    }

    /// Reads a fixed-width NUL-terminated UTF-8 string of `width` bytes.
    fn cstr(&mut self, width: usize, field: &'static str) -> Result<String, DbnError> {
        let offset = self.base + self.pos as u64;
        let bytes = self.bytes(width);
        let Some(end) = bytes.iter().position(|&b| b == 0) else {
            return Err(DbnError::InvalidCString {
                field,
                offset,
                reason: "no NUL terminator",
            });
        };
        match std::str::from_utf8(&bytes[..end]) {
            Ok(s) => Ok(s.to_owned()),
            Err(_) => Err(DbnError::InvalidCString {
                field,
                offset,
                reason: "not valid UTF-8",
            }),
        }
    }
}

/// Appends `s` as a fixed-width NUL-terminated string of `width` bytes, cut at a character boundary if it
/// does not fit.
fn put_cstr(out: &mut Vec<u8>, s: &str, width: usize) {
    let mut n = s.len().min(width.saturating_sub(1));
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    out.extend_from_slice(&s.as_bytes()[..n]);
    put_zeros(out, width - n);
}

fn put_zeros(out: &mut Vec<u8>, n: usize) {
    out.resize(out.len() + n, 0);
}

fn invalid_metadata(reason: String) -> DbnError {
    DbnError::InvalidMetadata { reason }
}

/// Reads a `u32` count of items of at least `item_len` bytes each, checking that they fit in the rest of
/// the metadata.
fn read_count(c: &mut Cursor<'_>, item_len: usize, what: &str) -> Result<usize, DbnError> {
    if c.remaining() < 4 {
        return Err(invalid_metadata(format!("the metadata ends before the {what} count")));
    }
    let count = c.num::<u32>() as usize;
    if count > c.remaining() / item_len {
        return Err(invalid_metadata(format!("{count} {what} entries do not fit in the remaining {} metadata bytes", c.remaining())));
    }
    Ok(count)
}

fn read_cstr_list(c: &mut Cursor<'_>, width: usize, field: &'static str) -> Result<Vec<String>, DbnError> {
    let count = read_count(c, width, field)?;
    (0..count).map(|_| c.cstr(width, field)).collect()
}

/// Parses the metadata after the 8-byte prefix.
fn parse_metadata(version: u8, body: &[u8]) -> Result<Metadata, DbnError> {
    if body.len() < METADATA_FIXED_LEN + 4 {
        return Err(invalid_metadata(format!(
            "the metadata length {} is shorter than its {}-byte fixed part",
            body.len(),
            METADATA_FIXED_LEN + 4
        )));
    }
    let mut c = Cursor::new(body, METADATA_PREFIX_LEN as u64);
    let dataset = c.cstr(DATASET_CSTR_LEN, "Metadata::dataset")?;
    let schema = Some(c.num::<u16>()).filter(|&s| s != u16::MAX);
    let start = c.num::<u64>();
    let end = Some(c.num::<u64>()).filter(|&e| e != 0 && e != UNDEF_TIMESTAMP);
    let limit = Some(c.num::<u64>()).filter(|&l| l != 0);
    if version == 1 {
        c.skip(8); // record_count
    }
    let stype_in = Some(c.num::<u8>()).filter(|&s| s != u8::MAX);
    let stype_out = c.num::<u8>();
    let ts_out = c.num::<u8>() != 0;
    let symbol_cstr_len = if version == 1 { SYMBOL_CSTR_LEN_V1 } else { usize::from(c.num::<u16>()) };
    c.skip(if version == 1 { METADATA_RESERVED_LEN_V1 } else { METADATA_RESERVED_LEN_V2 });
    let schema_definition_len = c.num::<u32>() as usize;
    if schema_definition_len > c.remaining() {
        return Err(invalid_metadata(format!("the {schema_definition_len}-byte schema definition does not fit in the metadata")));
    }
    c.skip(schema_definition_len);
    if symbol_cstr_len == 0 {
        return Err(invalid_metadata("the symbol width is 0".to_owned()));
    }
    let symbols = read_cstr_list(&mut c, symbol_cstr_len, "Metadata::symbols")?;
    let partial = read_cstr_list(&mut c, symbol_cstr_len, "Metadata::partial")?;
    let not_found = read_cstr_list(&mut c, symbol_cstr_len, "Metadata::not_found")?;
    let mapping_count = read_count(&mut c, symbol_cstr_len + 4, "Metadata::mappings")?;
    let mut mappings = Vec::with_capacity(mapping_count);
    for _ in 0..mapping_count {
        if c.remaining() < symbol_cstr_len + 4 {
            return Err(invalid_metadata(format!("the metadata ends inside the {mapping_count} symbol mappings")));
        }
        let raw_symbol = c.cstr(symbol_cstr_len, "SymbolMapping::raw_symbol")?;
        let interval_count = read_count(&mut c, symbol_cstr_len + 8, "SymbolMapping::intervals")?;
        let mut intervals = Vec::with_capacity(interval_count);
        for _ in 0..interval_count {
            let start_date = c.num::<u32>();
            let end_date = c.num::<u32>();
            let symbol = c.cstr(symbol_cstr_len, "MappingInterval::symbol")?;
            intervals.push(MappingInterval { start_date, end_date, symbol });
        }
        mappings.push(SymbolMapping { raw_symbol, intervals });
    }
    Ok(Metadata {
        version,
        dataset,
        schema,
        start,
        end,
        limit,
        stype_in,
        stype_out,
        ts_out,
        symbol_cstr_len,
        symbols,
        partial,
        not_found,
        mappings,
    })
}

fn encode_metadata(m: &Metadata, out: &mut Vec<u8>) {
    let version = m.version;
    let width = if version == 1 { SYMBOL_CSTR_LEN_V1 } else { m.symbol_cstr_len };
    assert!(
        width > 0 && width <= usize::from(u16::MAX),
        "DBN metadata symbol_cstr_len {width} cannot be encoded: it has to be between 1 and 65535"
    );
    out.extend_from_slice(b"DBN");
    out.push(version);
    let length_pos = out.len();
    put_zeros(out, 4);
    let body_start = out.len();
    put_cstr(out, &m.dataset, DATASET_CSTR_LEN);
    m.schema.unwrap_or(u16::MAX).put(out);
    m.start.put(out);
    m.end.unwrap_or(UNDEF_TIMESTAMP).put(out);
    m.limit.unwrap_or(0).put(out);
    if version == 1 {
        u64::MAX.put(out); // record_count
    }
    m.stype_in.unwrap_or(u8::MAX).put(out);
    m.stype_out.put(out);
    u8::from(m.ts_out).put(out);
    if version > 1 {
        (width as u16).put(out);
        put_zeros(out, METADATA_RESERVED_LEN_V2);
    } else {
        put_zeros(out, METADATA_RESERVED_LEN_V1);
    }
    0u32.put(out); // schema definition length
    for list in [&m.symbols, &m.partial, &m.not_found] {
        (list.len() as u32).put(out);
        for symbol in list {
            put_cstr(out, symbol, width);
        }
    }
    (m.mappings.len() as u32).put(out);
    for mapping in &m.mappings {
        put_cstr(out, &mapping.raw_symbol, width);
        (mapping.intervals.len() as u32).put(out);
        for interval in &mapping.intervals {
            interval.start_date.put(out);
            interval.end_date.put(out);
            put_cstr(out, &interval.symbol, width);
        }
    }
    if version >= 3 {
        let unaligned = (out.len() - body_start) % 8;
        if unaligned != 0 {
            put_zeros(out, 8 - unaligned);
        }
    }
    let length = (out.len() - body_start) as u32;
    out[length_pos..body_start].copy_from_slice(&length.to_le_bytes());
}

/// Converts a version 1 or 2 statistics quantity to the version 3 width.
fn widen_quantity(quantity: i32) -> i64 {
    if quantity == i32::MAX {
        UNDEF_STAT_QUANTITY
    } else {
        i64::from(quantity)
    }
}

/// Converts a statistics quantity to the version 1 and 2 width; values outside the `i32` range become the
/// undefined `i32::MAX`.
fn narrow_quantity(quantity: i64) -> i32 {
    i32::try_from(quantity).unwrap_or(i32::MAX)
}

/// Maps the `u8::MAX` sentinel to `None`.
fn defined_u8(value: u8) -> Option<u8> {
    Some(value).filter(|&v| v != u8::MAX)
}

/// Decodes a complete record of `version` from `b`, whose length has been checked against the layout.
/// `offset` is the position of the record in the input, for error messages.
fn decode_record(version: u8, b: &[u8], offset: u64) -> Result<Record, DbnError> {
    let mut c = Cursor::new(b, offset);
    c.skip(1); // length
    let hd = Header {
        rtype: c.num(),
        publisher_id: c.num(),
        instrument_id: c.num(),
        ts_event: c.num(),
    };
    let symbol_width = if version == 1 { SYMBOL_CSTR_LEN_V1 } else { SYMBOL_CSTR_LEN_V2 };
    let record = match hd.rtype {
        rtype::OHLCV_DEPRECATED | rtype::OHLCV_1S..=rtype::OHLCV_EOD => Record::Ohlcv {
            hd,
            open: c.num(),
            high: c.num(),
            low: c.num(),
            close: c.num(),
            volume: c.num(),
        },
        rtype::INSTRUMENT_DEF => {
            let mut def = InstrumentDef { hd, ..InstrumentDef::default() };
            match version {
                1 => decode_def_v1(&mut c, &mut def)?,
                2 => decode_def_v2(&mut c, &mut def)?,
                _ => decode_def_v3(&mut c, &mut def)?,
            }
            Record::InstrumentDef(Box::new(def))
        }
        rtype::STATISTICS => {
            let ts_recv = c.num();
            let ts_ref = c.num();
            let price = c.num();
            let quantity = if version < 3 { widen_quantity(c.num()) } else { c.num() };
            Record::Statistics {
                hd,
                ts_recv,
                ts_ref,
                price,
                quantity,
                sequence: c.num(),
                ts_in_delta: c.num(),
                stat_type: c.num(),
                channel_id: c.num(),
                update_action: c.num(),
                stat_flags: c.num(),
            }
        }
        rtype::SYMBOL_MAPPING => {
            let stype_in = if version == 1 { None } else { defined_u8(c.num()) };
            let stype_in_symbol = c.cstr(symbol_width, "SymbolMapping::stype_in_symbol")?;
            let stype_out = if version == 1 { None } else { defined_u8(c.num()) };
            let stype_out_symbol = c.cstr(symbol_width, "SymbolMapping::stype_out_symbol")?;
            if version == 1 {
                c.skip(4);
            }
            Record::SymbolMapping {
                hd,
                stype_in,
                stype_in_symbol,
                stype_out,
                stype_out_symbol,
                start_ts: c.num(),
                end_ts: c.num(),
            }
        }
        rtype::ERROR if version == 1 => Record::Error {
            hd,
            err: c.cstr(64, "Error::err")?,
            code: None,
            is_last: None,
        },
        rtype::ERROR => {
            let err = c.cstr(302, "Error::err")?;
            let code = defined_u8(c.num());
            let is_last = defined_u8(c.num()).map(|v| v != 0);
            Record::Error { hd, err, code, is_last }
        }
        rtype::SYSTEM if version == 1 => Record::System {
            hd,
            msg: c.cstr(64, "System::msg")?,
            code: None,
        },
        rtype::SYSTEM => {
            let msg = c.cstr(303, "System::msg")?;
            Record::System { hd, msg, code: defined_u8(c.num()) }
        }
        _ => Record::Other {
            hd,
            body: b.get(HEADER_LEN..).unwrap_or_default().to_vec(),
        },
    };
    Ok(record)
}

/// Appends `rec` in the layout of `version`, followed by `ts_out` when given.
fn encode_record(version: u8, rec: &Record, ts_out: Option<u64>, out: &mut Vec<u8>) {
    let start = out.len();
    let hd = rec.header();
    out.push(0); // length, set below
    hd.rtype.put(out);
    hd.publisher_id.put(out);
    hd.instrument_id.put(out);
    hd.ts_event.put(out);
    let symbol_width = if version == 1 { SYMBOL_CSTR_LEN_V1 } else { SYMBOL_CSTR_LEN_V2 };
    match rec {
        Record::Ohlcv { open, high, low, close, volume, .. } => {
            open.put(out);
            high.put(out);
            low.put(out);
            close.put(out);
            volume.put(out);
        }
        Record::InstrumentDef(def) => match version {
            1 => encode_def_v1(def, out),
            2 => encode_def_v2(def, out),
            _ => encode_def_v3(def, out),
        },
        Record::Statistics {
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
            ..
        } => {
            ts_recv.put(out);
            ts_ref.put(out);
            price.put(out);
            if version < 3 {
                narrow_quantity(*quantity).put(out);
            } else {
                quantity.put(out);
            }
            sequence.put(out);
            ts_in_delta.put(out);
            stat_type.put(out);
            channel_id.put(out);
            update_action.put(out);
            stat_flags.put(out);
            put_zeros(out, if version < 3 { 6 } else { 18 });
        }
        Record::SymbolMapping {
            stype_in,
            stype_in_symbol,
            stype_out,
            stype_out_symbol,
            start_ts,
            end_ts,
            ..
        } => {
            if version > 1 {
                stype_in.unwrap_or(u8::MAX).put(out);
            }
            put_cstr(out, stype_in_symbol, symbol_width);
            if version > 1 {
                stype_out.unwrap_or(u8::MAX).put(out);
            }
            put_cstr(out, stype_out_symbol, symbol_width);
            if version == 1 {
                put_zeros(out, 4);
            }
            start_ts.put(out);
            end_ts.put(out);
        }
        Record::Error { err, code, is_last, .. } => {
            if version == 1 {
                put_cstr(out, err, 64);
            } else {
                put_cstr(out, err, 302);
                code.unwrap_or(u8::MAX).put(out);
                is_last.map_or(u8::MAX, u8::from).put(out);
            }
        }
        Record::System { msg, code, .. } => {
            if version == 1 {
                put_cstr(out, msg, 64);
            } else {
                put_cstr(out, msg, 303);
                code.unwrap_or(u8::MAX).put(out);
            }
        }
        Record::Other { body, .. } => {
            out.extend_from_slice(body);
            let unaligned = body.len() % LENGTH_MULTIPLIER;
            if unaligned != 0 {
                put_zeros(out, LENGTH_MULTIPLIER - unaligned);
            }
        }
    }
    if let Some(ts_out) = ts_out {
        ts_out.put(out);
    }
    let length = out.len() - start;
    assert!(length <= MAX_LENGTH, "a {length}-byte DBN record cannot be encoded: a record holds at most {MAX_LENGTH} bytes");
    out[start] = (length / LENGTH_MULTIPLIER) as u8;
}

/// Decodes or encodes one field of an instrument definition layout; see `def_layout!`.
macro_rules! def_field {
    (decode, $c:ident, $d:ident, _, pad, $n:expr) => {
        $c.skip($n)
    };
    (encode, $o:ident, $d:ident, _, pad, $n:expr) => {
        put_zeros($o, $n)
    };
    (decode, $c:ident, $d:ident, $f:ident, num) => {
        $d.$f = $c.num()
    };
    (encode, $o:ident, $d:ident, $f:ident, num) => {
        $d.$f.put($o)
    };
    (decode, $c:ident, $d:ident, $f:ident, u32) => {
        $d.$f = u64::from($c.num::<u32>())
    };
    (encode, $o:ident, $d:ident, $f:ident, u32) => {
        u32::try_from($d.$f).unwrap_or(0).put($o)
    };
    (decode, $c:ident, $d:ident, $f:ident, cstr, $n:expr) => {
        $d.$f = $c.cstr($n, concat!("InstrumentDef::", stringify!($f)))?
    };
    (encode, $o:ident, $d:ident, $f:ident, cstr, $n:expr) => {
        put_cstr($o, &$d.$f, $n)
    };
}

/// Defines the decoder and the encoder of an instrument definition layout from its fields in wire order
/// after the header. Field kinds: `num` (an integer of the field's type), `u32` (a `u64` field stored as
/// `u32`), `cstr(width)` (a fixed-width string) and `pad(n)` (`n` reserved bytes, named `_`).
macro_rules! def_layout {
    ($decode:ident, $encode:ident, { $($field:tt : $kind:ident $(($arg:expr))?),* $(,)? }) => {
        fn $decode(c: &mut Cursor<'_>, d: &mut InstrumentDef) -> Result<(), DbnError> {
            $(def_field!(decode, c, d, $field, $kind $(, $arg)?);)*
            Ok(())
        }

        fn $encode(d: &InstrumentDef, out: &mut Vec<u8>) {
            $(def_field!(encode, out, d, $field, $kind $(, $arg)?);)*
        }
    };
}

// Version 1: 360 bytes.
def_layout!(decode_def_v1, encode_def_v1, {
    ts_recv: num, min_price_increment: num, display_factor: num, expiration: num, activation: num, high_limit_price: num, low_limit_price: num,
    max_price_variation: num, trading_reference_price: num, unit_of_measure_qty: num, min_price_increment_amount: num, price_ratio: num,
    inst_attrib_value: num, underlying_id: num, raw_instrument_id: u32, market_depth_implied: num, market_depth: num, market_segment_id: num,
    max_trade_vol: num, min_lot_size: num, min_lot_size_block: num, min_lot_size_round_lot: num, min_trade_vol: num, _: pad(4),
    contract_multiplier: num, decay_quantity: num, original_contract_size: num, _: pad(4), trading_reference_date: num, appl_id: num,
    maturity_year: num, decay_start_date: num, channel_id: num, currency: cstr(4), settl_currency: cstr(4), secsubtype: cstr(6),
    raw_symbol: cstr(SYMBOL_CSTR_LEN_V1), group: cstr(21), exchange: cstr(5), asset: cstr(7), cfi: cstr(7), security_type: cstr(7),
    unit_of_measure: cstr(31), underlying: cstr(21), strike_price_currency: cstr(4), instrument_class: num, _: pad(2), strike_price: num,
    _: pad(6), match_algorithm: num, md_security_trading_status: num, main_fraction: num, price_display_format: num, settl_price_type: num,
    sub_fraction: num, underlying_product: num, security_update_action: num, maturity_month: num, maturity_day: num, maturity_week: num,
    user_defined_instrument: num, contract_multiplier_unit: num, flow_schedule_type: num, tick_rule: num, _: pad(3),
});

// Version 2: 400 bytes.
def_layout!(decode_def_v2, encode_def_v2, {
    ts_recv: num, min_price_increment: num, display_factor: num, expiration: num, activation: num, high_limit_price: num, low_limit_price: num,
    max_price_variation: num, trading_reference_price: num, unit_of_measure_qty: num, min_price_increment_amount: num, price_ratio: num,
    strike_price: num, inst_attrib_value: num, underlying_id: num, raw_instrument_id: u32, market_depth_implied: num, market_depth: num,
    market_segment_id: num, max_trade_vol: num, min_lot_size: num, min_lot_size_block: num, min_lot_size_round_lot: num, min_trade_vol: num,
    contract_multiplier: num, decay_quantity: num, original_contract_size: num, trading_reference_date: num, appl_id: num, maturity_year: num,
    decay_start_date: num, channel_id: num, currency: cstr(4), settl_currency: cstr(4), secsubtype: cstr(6), raw_symbol: cstr(SYMBOL_CSTR_LEN_V2),
    group: cstr(21), exchange: cstr(5), asset: cstr(7), cfi: cstr(7), security_type: cstr(7), unit_of_measure: cstr(31), underlying: cstr(21),
    strike_price_currency: cstr(4), instrument_class: num, match_algorithm: num, md_security_trading_status: num, main_fraction: num,
    price_display_format: num, settl_price_type: num, sub_fraction: num, underlying_product: num, security_update_action: num,
    maturity_month: num, maturity_day: num, maturity_week: num, user_defined_instrument: num, contract_multiplier_unit: num,
    flow_schedule_type: num, tick_rule: num, _: pad(10),
});

// Version 3: 520 bytes.
def_layout!(decode_def_v3, encode_def_v3, {
    ts_recv: num, min_price_increment: num, display_factor: num, expiration: num, activation: num, high_limit_price: num, low_limit_price: num,
    max_price_variation: num, unit_of_measure_qty: num, min_price_increment_amount: num, price_ratio: num, strike_price: num,
    raw_instrument_id: num, leg_price: num, leg_delta: num, inst_attrib_value: num, underlying_id: num, market_depth_implied: num,
    market_depth: num, market_segment_id: num, max_trade_vol: num, min_lot_size: num, min_lot_size_block: num, min_lot_size_round_lot: num,
    min_trade_vol: num, contract_multiplier: num, decay_quantity: num, original_contract_size: num, leg_instrument_id: num,
    leg_ratio_price_numerator: num, leg_ratio_price_denominator: num, leg_ratio_qty_numerator: num, leg_ratio_qty_denominator: num,
    leg_underlying_id: num, appl_id: num, maturity_year: num, decay_start_date: num, channel_id: num, leg_count: num, leg_index: num,
    currency: cstr(4), settl_currency: cstr(4), secsubtype: cstr(6), raw_symbol: cstr(SYMBOL_CSTR_LEN_V2), group: cstr(21), exchange: cstr(5),
    asset: cstr(11), cfi: cstr(7), security_type: cstr(7), unit_of_measure: cstr(31), underlying: cstr(21), strike_price_currency: cstr(4),
    leg_raw_symbol: cstr(SYMBOL_CSTR_LEN_V2), instrument_class: num, match_algorithm: num, main_fraction: num, price_display_format: num,
    sub_fraction: num, underlying_product: num, security_update_action: num, maturity_month: num, maturity_day: num, maturity_week: num,
    user_defined_instrument: num, contract_multiplier_unit: num, flow_schedule_type: num, tick_rule: num, leg_instrument_class: num,
    leg_side: num, _: pad(17),
});

#[cfg(test)]
mod tests {
    use super::*;

    fn hd(rtype: u8) -> Header {
        Header {
            rtype,
            publisher_id: 1,
            instrument_id: 2,
            ts_event: 3,
        }
    }

    fn sample_records() -> Vec<Record> {
        vec![
            Record::Ohlcv {
                hd: hd(rtype::OHLCV_1M),
                open: 1,
                high: 2,
                low: 3,
                close: 4,
                volume: 5,
            },
            Record::InstrumentDef(Box::default()),
            Record::Statistics {
                hd: hd(rtype::STATISTICS),
                ts_recv: 1,
                ts_ref: 2,
                price: 3,
                quantity: 4,
                sequence: 5,
                ts_in_delta: 6,
                stat_type: 7,
                channel_id: 8,
                update_action: 1,
                stat_flags: 9,
            },
            Record::SymbolMapping {
                hd: hd(rtype::SYMBOL_MAPPING),
                stype_in: Some(1),
                stype_in_symbol: "ES.c.0".to_owned(),
                stype_out: Some(0),
                stype_out_symbol: "ESH4".to_owned(),
                start_ts: 10,
                end_ts: 20,
            },
            Record::Error {
                hd: hd(rtype::ERROR),
                err: "bad".to_owned(),
                code: Some(4),
                is_last: Some(true),
            },
            Record::System {
                hd: hd(rtype::SYSTEM),
                msg: "Heartbeat".to_owned(),
                code: Some(0),
            },
            Record::Other { hd: hd(0xA0), body: vec![7; 40] },
        ]
    }

    fn metadata(version: u8, ts_out: bool) -> Metadata {
        Metadata {
            version,
            dataset: "GLBX.MDP3".to_owned(),
            schema: None,
            start: 1,
            end: None,
            limit: None,
            stype_in: None,
            stype_out: 0,
            ts_out,
            symbol_cstr_len: if version == 1 { SYMBOL_CSTR_LEN_V1 } else { SYMBOL_CSTR_LEN_V2 },
            symbols: vec![],
            partial: vec![],
            not_found: vec![],
            mappings: vec![],
        }
    }

    #[test]
    fn record_lengths_match_the_layouts() {
        let expected: [(u8, [usize; 7]); 3] = [(1, [56, 360, 64, 80, 80, 80, 56]), (2, [56, 400, 64, 176, 320, 320, 56]), (3, [56, 520, 80, 176, 320, 320, 56])];
        for (version, lengths) in expected {
            for (record, expected_len) in sample_records().iter().zip(lengths) {
                for ts_out in [None, Some(99)] {
                    let mut out = Vec::new();
                    encode_record(version, record, ts_out, &mut out);
                    let len = expected_len + if ts_out.is_some() { TS_OUT_LEN } else { 0 };
                    assert_eq!(out.len(), len, "v{version} {record:?}");
                    assert_eq!(usize::from(out[0]) * LENGTH_MULTIPLIER, len);
                    if !matches!(record, Record::Other { .. }) {
                        assert_eq!(layout_len(version, record.header().rtype), expected_len);
                    }
                }
            }
        }
    }

    #[test]
    fn records_round_trip_in_every_version() {
        for version in 1..=3 {
            for ts_out in [false, true] {
                let meta = metadata(version, ts_out);
                let records: Vec<Decoded> = sample_records()
                    .into_iter()
                    .map(|record| match record {
                        // Version 1 lacks these fields, so they decode as `None`.
                        Record::SymbolMapping {
                            hd,
                            stype_in_symbol,
                            stype_out_symbol,
                            start_ts,
                            end_ts,
                            ..
                        } if version == 1 => Record::SymbolMapping {
                            hd,
                            stype_in: None,
                            stype_in_symbol,
                            stype_out: None,
                            stype_out_symbol,
                            start_ts,
                            end_ts,
                        },
                        Record::Error { hd, err, .. } if version == 1 => Record::Error { hd, err, code: None, is_last: None },
                        Record::System { hd, msg, .. } if version == 1 => Record::System { hd, msg, code: None },
                        other => other,
                    })
                    .map(|record| Decoded { record, ts_out: ts_out.then_some(42) })
                    .collect();
                let bytes = encode(&meta, &records);
                let (decoded_meta, decoded) = decode(&bytes).unwrap();
                assert_eq!(decoded_meta, meta);
                assert_eq!(decoded, records);
                assert_eq!(encode(&decoded_meta, &decoded), bytes);
            }
        }
    }

    #[test]
    fn version_3_metadata_is_padded_to_8_bytes() {
        for version in 1..=3 {
            let mut meta = metadata(version, false);
            meta.symbols = vec!["A".to_owned()];
            let bytes = encode(&meta, &[]);
            let length = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
            assert_eq!(bytes.len(), 8 + length);
            let unpadded = 100 + 20 + meta.symbol_cstr_len;
            if version == 3 {
                assert_eq!(length, unpadded.div_ceil(8) * 8);
            } else {
                assert_eq!(length, unpadded);
            }
        }
    }

    #[test]
    fn strings_are_cut_at_character_boundaries() {
        let mut out = Vec::new();
        put_cstr(&mut out, "abc\u{e9}", 5); // "abc" + 2-byte character; 4 bytes fit
        assert_eq!(out, b"abc\0\0");
        let mut c = Cursor::new(&out, 0);
        assert_eq!(c.cstr(5, "f").unwrap(), "abc");
    }

    #[test]
    fn statistics_quantity_widens_and_narrows() {
        assert_eq!(widen_quantity(i32::MAX), UNDEF_STAT_QUANTITY);
        assert_eq!(widen_quantity(-5), -5);
        assert_eq!(narrow_quantity(UNDEF_STAT_QUANTITY), i32::MAX);
        assert_eq!(narrow_quantity(i64::from(i32::MIN) - 1), i32::MAX);
        assert_eq!(narrow_quantity(7), 7);
    }

    #[test]
    fn price_converts_fixed_point() {
        assert_eq!(price(UNDEF_PRICE), None);
        assert_eq!(price(1_500_000_000), Some(1.5));
        assert_eq!(price(-250_000_000), Some(-0.25));
    }
}
