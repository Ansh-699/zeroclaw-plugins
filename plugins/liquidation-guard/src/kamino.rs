//! Kamino Lend API payload parsing, obligation→facts mapping, and the
//! snapshot codec.
//!
//! Pure parsing + mapping only: no I/O lives here (the `net` module fetches;
//! this module turns response bodies into plain facts). Every JSON payload
//! shape below was verified against a live capture (see
//! `tests/fixtures/*.json`); every numeric field in the Kamino API arrives
//! as a JSON **string** and is parsed explicitly — never trust the wire
//! type.

use serde::{Deserialize, Serialize};

/// All-zero system-program pubkey used by Kamino as a "no reserve" /
/// "no referrer" placeholder sentinel.
const ZERO_PUBKEY: &str = "11111111111111111111111111111111";

// ---------------------------------------------------------------------
// Public facts types (interface contract — frozen; downstream slices
// compile against these).
// ---------------------------------------------------------------------

/// One obligation, mapped from the raw API payload to plain facts.
#[derive(Debug, Clone)]
pub struct ObligationFacts {
    pub obligation: String,
    pub market: String,
    pub owner: String,
    /// Fraction (not percent): `userTotalBorrowBorrowFactorAdjusted /
    /// userTotalLiquidatableDeposit`.
    pub ltv: f64,
    /// Fraction (not percent): `refreshedStats.liquidationLtv`.
    pub liq_ltv: f64,
    /// `userTotalBorrowBorrowFactorAdjusted`.
    pub borrow_usd: f64,
    /// `userTotalLiquidatableDeposit`.
    pub deposit_usd: f64,
    /// `None` when `state.referrer` is the all-zero sentinel.
    pub referrer: Option<String>,
    pub elevation_group: u8,
    /// `state.deposits`, fixed-size placeholder rows filtered out.
    pub deposits: Vec<PositionRow>,
    /// `state.borrows`, fixed-size placeholder rows filtered out.
    pub borrows: Vec<PositionRow>,
    /// `market.state.autodeleverageEnabled` (market-level, not per-reserve).
    pub market_adl_enabled: bool,
    /// `market.state.minFullLiquidationValueThreshold`; `None` when the
    /// payload doesn't carry it — the dust check is suppressed, never
    /// defaulted.
    pub min_full_liquidation_value_usd: Option<f64>,
}

/// One real (non-placeholder) deposit or borrow row.
#[derive(Debug, Clone)]
pub struct PositionRow {
    pub reserve: String,
    /// `marketValueSf / 2^60` — last-crank composition value; use only to
    /// compare rows within a position, never as a total (totals come from
    /// `refreshedStats`).
    pub usd_value: f64,
    /// Raw on-chain amount, passed through as a string (deposits:
    /// `depositedAmount`; borrows: `borrowedAmountSf`) — not interpreted
    /// here.
    pub raw_amount: String,
}

/// One row of `/oracles/prices`.
#[derive(Debug, Clone)]
pub struct PriceRow {
    pub mint: String,
    pub name: String,
    pub price: f64,
    pub timestamp: i64,
    pub max_age_s: i64,
}

/// One row of `/kamino-market/{m}/reserves/metrics` — the only
/// reserve→mint/symbol mapping source.
#[derive(Debug, Clone)]
pub struct ReserveMetrics {
    pub reserve: String,
    pub mint: String,
    pub symbol: String,
    pub borrow_apy: f64,
    /// `totalBorrow / totalSupply`, `None` when supply is zero.
    pub utilization: Option<f64>,
}

/// Opaque, versioned snapshot carried across calls by the caller
/// (`prev_snapshot`). All fields public but callers must treat the encoded
/// string as opaque.
///
/// `obligation` binds a snapshot to the specific obligation it was taken
/// from (F6): the caller filters a decoded snapshot against the obligation
/// under assessment and drops it (falls back to "no prior snapshot") on any
/// mismatch, so a snapshot from one obligation can never be diffed against
/// another. Old-format snapshots that predate this field simply fail to
/// deserialize (a required field is missing), which already degrades to
/// `None` via `decode_snapshot`'s any-failure-is-None contract — no version
/// bump needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub v: u8,
    pub obligation: String,
    pub ltv: f64,
    pub liq_ltv: f64,
    pub collateral_price: f64,
    pub elevation_group: u8,
    pub taken_unix: i64,
}

// ---------------------------------------------------------------------
// Raw wire shapes (private — tolerant to extra fields by construction:
// no `deny_unknown_fields`, and only the fields actually consumed are
// declared).
// ---------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawObligation {
    obligation_address: String,
    market: RawMarket,
    refreshed_stats: RawRefreshedStats,
    state: RawState,
    // NOTE: the top-level `deposits`/`borrows` keys on this payload are
    // always empty objects `{}` — intentionally not mapped to a field
    // here; the real rows live at `state.deposits` / `state.borrows`.
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMarket {
    address: String,
    state: RawMarketState,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMarketState {
    /// JSON number (`0`/`1`), not a JSON bool — Kamino's wire shape.
    autodeleverage_enabled: u8,
    /// JSON string, like every Kamino numeric; `None` when the payload
    /// omits the key (missing != zero — never defaulted).
    min_full_liquidation_value_threshold: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRefreshedStats {
    liquidation_ltv: String,
    user_total_borrow_borrow_factor_adjusted: String,
    user_total_liquidatable_deposit: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawState {
    owner: String,
    referrer: String,
    elevation_group: u8,
    deposits: Vec<RawDeposit>,
    borrows: Vec<RawBorrow>,
    // NOTE (safety invariant 8): `state.lastUpdate.stale` is the on-chain
    // inter-crank marker — a live capture observed it as `1` on a fully
    // healthy obligation, so it is never a liquidation-risk signal and is
    // intentionally not deserialized or branched on anywhere in this
    // module. The only staleness clock is the prices-response HTTP `Date`
    // header vs. each price row's own `timestamp`/`maxAgeInSeconds` (see
    // `http_date_to_unix` / `price_is_stale`).
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDeposit {
    deposit_reserve: String,
    deposited_amount: String,
    market_value_sf: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBorrow {
    borrow_reserve: String,
    borrowed_amount_sf: String,
    market_value_sf: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPrice {
    mint: String,
    name: String,
    max_age_in_seconds: String,
    price: String,
    timestamp: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReserveMetric {
    reserve: String,
    liquidity_token: String,
    liquidity_token_mint: String,
    borrow_apy: String,
    total_borrow: String,
    total_supply: String,
}

// ---------------------------------------------------------------------
// Parsing entry points.
// ---------------------------------------------------------------------

/// Parses an `/obligations` list response body into plain facts.
///
/// Tolerant to extra payload fields; a required field missing from any row
/// produces a typed `Err` naming that field (via serde's own message).
pub fn parse_obligations(body: &str) -> Result<Vec<ObligationFacts>, String> {
    let raw: Vec<RawObligation> = serde_json::from_str(body).map_err(|e| e.to_string())?;
    raw.into_iter().map(map_obligation).collect()
}

/// Parses an `/oracles/prices` list response body.
pub fn parse_prices(body: &str) -> Result<Vec<PriceRow>, String> {
    let raw: Vec<RawPrice> = serde_json::from_str(body).map_err(|e| e.to_string())?;
    raw.into_iter().map(map_price).collect()
}

/// Parses a `/reserves/metrics` list response body.
pub fn parse_reserves_metrics(body: &str) -> Result<Vec<ReserveMetrics>, String> {
    let raw: Vec<RawReserveMetric> = serde_json::from_str(body).map_err(|e| e.to_string())?;
    raw.into_iter().map(map_reserve_metric).collect()
}

fn map_obligation(raw: RawObligation) -> Result<ObligationFacts, String> {
    let borrow_usd = parse_num(
        &raw.refreshed_stats.user_total_borrow_borrow_factor_adjusted,
        "userTotalBorrowBorrowFactorAdjusted",
    )?;
    let deposit_usd = parse_num(
        &raw.refreshed_stats.user_total_liquidatable_deposit,
        "userTotalLiquidatableDeposit",
    )?;
    let liq_ltv = parse_num(&raw.refreshed_stats.liquidation_ltv, "liquidationLtv")?;
    // LTV for all health math is the BF-adjusted ratio, not the payload's
    // own `loanToValue` field (equal when every borrow has BF=1).
    let ltv = if deposit_usd != 0.0 {
        borrow_usd / deposit_usd
    } else {
        0.0
    };

    let referrer = if raw.state.referrer == ZERO_PUBKEY {
        None
    } else {
        Some(raw.state.referrer)
    };

    let market_adl_enabled = raw.market.state.autodeleverage_enabled != 0;
    let min_full_liquidation_value_usd = raw
        .market
        .state
        .min_full_liquidation_value_threshold
        .as_deref()
        .map(|s| parse_num(s, "minFullLiquidationValueThreshold"))
        .transpose()?;

    let deposits = raw
        .state
        .deposits
        .into_iter()
        .filter(|d| d.deposit_reserve != ZERO_PUBKEY)
        .map(|d| {
            Ok(PositionRow {
                usd_value: sf_to_usd(&d.market_value_sf)?,
                reserve: d.deposit_reserve,
                raw_amount: d.deposited_amount,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let borrows = raw
        .state
        .borrows
        .into_iter()
        .filter(|b| b.borrow_reserve != ZERO_PUBKEY)
        .map(|b| {
            Ok(PositionRow {
                usd_value: sf_to_usd(&b.market_value_sf)?,
                reserve: b.borrow_reserve,
                raw_amount: b.borrowed_amount_sf,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(ObligationFacts {
        obligation: raw.obligation_address,
        market: raw.market.address,
        owner: raw.state.owner,
        ltv,
        liq_ltv,
        borrow_usd,
        deposit_usd,
        referrer,
        elevation_group: raw.state.elevation_group,
        deposits,
        borrows,
        market_adl_enabled,
        min_full_liquidation_value_usd,
    })
}

fn map_price(raw: RawPrice) -> Result<PriceRow, String> {
    Ok(PriceRow {
        price: parse_num(&raw.price, "price")?,
        timestamp: parse_int(&raw.timestamp, "timestamp")?,
        max_age_s: parse_int(&raw.max_age_in_seconds, "maxAgeInSeconds")?,
        mint: raw.mint,
        name: raw.name,
    })
}

fn map_reserve_metric(raw: RawReserveMetric) -> Result<ReserveMetrics, String> {
    let borrow_apy = parse_num(&raw.borrow_apy, "borrowApy")?;
    let total_borrow = parse_num(&raw.total_borrow, "totalBorrow")?;
    let total_supply = parse_num(&raw.total_supply, "totalSupply")?;
    let utilization = if total_supply != 0.0 {
        Some(total_borrow / total_supply)
    } else {
        None
    };
    Ok(ReserveMetrics {
        reserve: raw.reserve,
        mint: raw.liquidity_token_mint,
        symbol: raw.liquidity_token,
        borrow_apy,
        utilization,
    })
}

/// Parses a Kamino API decimal string into `f64`, naming the field on
/// failure.
fn parse_num(s: &str, field: &str) -> Result<f64, String> {
    s.parse::<f64>()
        .map_err(|_| format!("invalid `{field}` value: {s:?}"))
}

/// Parses a Kamino API integer string into `i64`, naming the field on
/// failure.
fn parse_int(s: &str, field: &str) -> Result<i64, String> {
    s.parse::<i64>()
        .map_err(|_| format!("invalid `{field}` value: {s:?}"))
}

/// `marketValueSf` (a decimal-string integer scaled by 2^60) to USD.
/// Precision loss from the f64 string-parse is fine for display math; per
/// spec these values are last-on-chain-crank and only ever used for
/// per-position composition, never totals.
fn sf_to_usd(sf: &str) -> Result<f64, String> {
    let raw = parse_num(sf, "marketValueSf")?;
    Ok(raw / (1u128 << 60) as f64)
}

// ---------------------------------------------------------------------
// Staleness clock: the wasm world has no clock, so the prices response's
// own HTTP `Date` header is the only time source (safety invariant 8).
// ---------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Parses an RFC-1123 HTTP `Date` header (e.g. `Sat, 18 Jul 2026 15:31:07
/// GMT`) into a unix timestamp. Hand-rolled over the fixed format the
/// Kamino API always sends — no chrono dependency.
pub fn http_date_to_unix(date_header: &str) -> Result<i64, String> {
    let parts: Vec<&str> = date_header.split_whitespace().collect();
    let [_dow, day, mon, year, time, tz] = parts[..] else {
        return Err(format!("malformed HTTP date: {date_header:?}"));
    };
    if tz != "GMT" {
        return Err(format!("expected GMT timezone: {date_header:?}"));
    }
    let day: i64 = day
        .parse()
        .map_err(|_| format!("bad day in HTTP date: {date_header:?}"))?;
    let month = MONTHS
        .iter()
        .position(|m| *m == mon)
        .ok_or_else(|| format!("bad month in HTTP date: {date_header:?}"))? as i64
        + 1;
    let year: i64 = year
        .parse()
        .map_err(|_| format!("bad year in HTTP date: {date_header:?}"))?;

    let mut hms = time.split(':');
    let (h, m, s) = (hms.next(), hms.next(), hms.next());
    let (Some(h), Some(m), Some(s)) = (h, m, s) else {
        return Err(format!("bad time in HTTP date: {date_header:?}"));
    };
    let hour: i64 = h
        .parse()
        .map_err(|_| format!("bad hour in HTTP date: {date_header:?}"))?;
    let min: i64 = m
        .parse()
        .map_err(|_| format!("bad minute in HTTP date: {date_header:?}"))?;
    let sec: i64 = s
        .parse()
        .map_err(|_| format!("bad second in HTTP date: {date_header:?}"))?;

    let days = days_from_civil(year, month, day);
    Ok(days * 86_400 + hour * 3600 + min * 60 + sec)
}

/// Howard Hinnant's `days_from_civil`: proleptic-Gregorian (year, month,
/// day) to days since the unix epoch. Public-domain algorithm; correct for
/// all dates the HTTP `Date` header can carry.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m + 9) % 12; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// True when a price row is stale as of `now_unix` (normally
/// `http_date_to_unix` of the same prices response's `Date` header).
pub fn price_is_stale(now_unix: i64, row: &PriceRow) -> bool {
    now_unix - row.timestamp > row.max_age_s
}

// ---------------------------------------------------------------------
// Snapshot codec.
// ---------------------------------------------------------------------

/// Encodes a snapshot to its opaque wire form. Never fails: on the
/// unreachable-in-practice case of a non-finite field, returns an empty
/// string, which `decode_snapshot` then treats as "no prior" like any
/// other garbled input.
pub fn encode_snapshot(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// Decodes a snapshot from its opaque wire form. A spec invariant: ANY
/// failure (garbled string, wrong version, truncated JSON) degrades to
/// `None` — "no prior snapshot" — never an `Err`; the caller's call still
/// succeeds.
pub fn decode_snapshot(s: &str) -> Option<Snapshot> {
    serde_json::from_str(s).ok()
}
