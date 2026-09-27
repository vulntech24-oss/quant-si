//! Instruments and daily candles (`docs/integrations/kite.md`, "Instruments and candles").

use std::collections::HashMap;
use std::str::FromStr;

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveTime, Offset, Utc};
use qd_domain::instrument::{InstrumentSpec, Venue};
use qd_domain::market::{Bar, BarData};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;

use crate::client::{KiteClient, KiteError};

/// India Standard Time.
#[must_use]
pub fn ist() -> FixedOffset {
    // 19 800 s is within range, so the fallback is never taken.
    FixedOffset::east_opt(19_800).unwrap_or_else(|| Utc.fix())
}

/// The Kite exchange code of a venue; `None` for venues Kite does not serve.
#[must_use]
pub fn exchange(venue: &Venue) -> Option<&'static str> {
    match venue {
        Venue::Nse => Some("NSE"),
        Venue::Bse => Some("BSE"),
        Venue::Nfo => Some("NFO"),
        Venue::Mcx => Some("MCX"),
        Venue::Crypto { .. } => None,
    }
}

/// Where an instrument trades at Kite: exchange, trading symbol and token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KiteRef {
    /// Exchange code.
    pub exchange: String,
    /// Kite trading symbol.
    pub tradingsymbol: String,
    /// Instrument token, when the spec records one.
    pub token: Option<String>,
}

/// The instrument's `kite` broker reference, if it has one on a Kite venue.
#[must_use]
pub fn kite_ref(spec: &InstrumentSpec) -> Option<KiteRef> {
    let exchange = exchange(&spec.venue)?;
    spec.broker_refs
        .iter()
        .find(|r| r.broker == "kite")
        .map(|r| KiteRef {
            exchange: exchange.to_owned(),
            tradingsymbol: r.symbol.clone(),
            token: r.token.clone().filter(|t| !t.is_empty()),
        })
}

/// One row of the instruments CSV.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct KiteInstrument {
    /// Token for candles.
    pub instrument_token: String,
    /// Trading symbol.
    pub tradingsymbol: String,
    /// Name.
    #[serde(default)]
    pub name: String,
    /// Expiry date (derivatives), empty otherwise.
    #[serde(default)]
    pub expiry: String,
    /// Tick size.
    pub tick_size: String,
    /// Lot size.
    pub lot_size: String,
    /// EQ, FUT, CE, PE.
    pub instrument_type: String,
    /// Segment.
    pub segment: String,
    /// Exchange.
    pub exchange: String,
}

/// Parses the instruments CSV (`GET /instruments[/:exchange]`).
pub fn parse_instruments(csv_text: &str) -> Result<Vec<KiteInstrument>, KiteError> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(csv_text.as_bytes());
    reader
        .deserialize()
        .map(|row| row.map_err(|e| KiteError::Unexpected(format!("instruments CSV: {e}"))))
        .collect()
}

/// Downloads one exchange's instruments.
pub async fn instruments(
    client: &KiteClient,
    exchange: &str,
) -> Result<Vec<KiteInstrument>, KiteError> {
    parse_instruments(&client.get_text(&format!("/instruments/{exchange}")).await?)
}

/// Tokens by (exchange, trading symbol).
#[must_use]
pub fn token_index(rows: &[KiteInstrument]) -> HashMap<(String, String), String> {
    rows.iter()
        .map(|r| {
            (
                (r.exchange.clone(), r.tradingsymbol.clone()),
                r.instrument_token.clone(),
            )
        })
        .collect()
}

/// A JSON number as an exact decimal (via its shortest text form, not a float).
pub(crate) fn decimal(value: &Value) -> Option<Decimal> {
    match value {
        Value::Number(n) => Decimal::from_str(&n.to_string())
            .or_else(|_| Decimal::from_scientific(&n.to_string()))
            .ok(),
        Value::String(s) => Decimal::from_str(s).ok(),
        _ => None,
    }
}

/// Parses `data.candles` rows `[timestamp, open, high, low, close, volume]`.
/// Candles dated after `complete_through` are still forming and are dropped.
pub fn parse_candles(data: &Value, complete_through: NaiveDate) -> Result<Vec<Bar>, KiteError> {
    let rows = data
        .get("candles")
        .and_then(Value::as_array)
        .ok_or_else(|| KiteError::Unexpected("candles missing".to_owned()))?;
    let mut bars = Vec::with_capacity(rows.len());
    for row in rows {
        let bad = || KiteError::Unexpected(format!("malformed candle {row}"));
        let cells = row.as_array().ok_or_else(bad)?;
        let stamp = cells.first().and_then(Value::as_str).ok_or_else(bad)?;
        let date = DateTime::parse_from_str(stamp, "%Y-%m-%dT%H:%M:%S%z")
            .map_err(|_| bad())?
            .with_timezone(&ist())
            .date_naive();
        if date > complete_through {
            continue;
        }
        let number = |i: usize| cells.get(i).and_then(decimal).ok_or_else(bad);
        let bar = Bar::new(BarData {
            date,
            open: number(1)?,
            high: number(2)?,
            low: number(3)?,
            close: number(4)?,
            volume: number(5)?,
        })
        .map_err(|e| KiteError::Unexpected(format!("candle {date}: {e}")))?;
        bars.push(bar);
    }
    Ok(bars)
}

/// Kite serves at most this many days of daily candles per request.
const MAX_DAYS_PER_REQUEST: i64 = 1_500;

/// Daily candles from `from` through `to`, in requests of at most
/// [`MAX_DAYS_PER_REQUEST`] days. `continuous` joins expired futures.
pub async fn daily_candles(
    client: &KiteClient,
    token: &str,
    from: NaiveDate,
    to: NaiveDate,
    continuous: bool,
    complete_through: NaiveDate,
) -> Result<Vec<Bar>, KiteError> {
    let mut bars = Vec::new();
    let mut start = from;
    while start <= to {
        let end = (start + Duration::days(MAX_DAYS_PER_REQUEST - 1)).min(to);
        let data = client
            .get(
                &format!("/instruments/historical/{token}/day"),
                &[
                    ("from", format!("{start} 00:00:00")),
                    ("to", format!("{end} 23:59:59")),
                    ("continuous", u8::from(continuous).to_string()),
                ],
            )
            .await?;
        bars.extend(parse_candles(&data, complete_through)?);
        start = end + Duration::days(1);
    }
    bars.sort_by_key(Bar::date);
    bars.dedup_by_key(|b| b.date());
    Ok(bars)
}

/// The last date whose daily bar is complete at `now` on the venue: today
/// once the session has closed (NSE, BSE, NFO 15:30 IST; MCX 23:30 IST),
/// with a margin for the exchange's closing data, else yesterday.
#[must_use]
pub fn completed_through(now: DateTime<Utc>, venue: &Venue) -> NaiveDate {
    let local = now.with_timezone(&ist());
    let done_at = match venue {
        Venue::Mcx => NaiveTime::from_hms_opt(23, 55, 0),
        _ => NaiveTime::from_hms_opt(16, 0, 0),
    }
    .unwrap_or(NaiveTime::MIN);
    let today = local.date_naive();
    if local.time() >= done_at {
        today
    } else {
        today.pred_opt().unwrap_or(today)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;
    use serde_json::json;

    #[test]
    fn candles_are_exact_decimals_on_the_ist_date_and_forming_bars_are_dropped() {
        let data = json!({"candles": [
            ["2017-12-15T00:00:00+0530", 1704.5, 1720, 1700.05, 1709.35, 1234],
            ["2017-12-18T00:00:00+0530", 1709.35, 1715.1, 1690, 1695.2, 99],
        ]});
        let day = NaiveDate::from_ymd_opt(2017, 12, 15).unwrap();
        let bars = parse_candles(&data, day).unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].date(), day);
        assert_eq!(bars[0].low().value(), dec!(1700.05));
        assert_eq!(bars[0].close().value(), dec!(1709.35));
        assert!(parse_candles(&json!({"candles": [["x", 1]]}), day).is_err());
    }

    #[test]
    fn a_day_counts_as_complete_after_the_venue_closes() {
        let at = |h, m| Utc.with_ymd_and_hms(2026, 9, 25, h, m, 0).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
        // 10:29 UTC is 15:59 IST; 10:30 UTC is 16:00 IST.
        assert_eq!(
            completed_through(at(10, 29), &Venue::Nse),
            today.pred_opt().unwrap()
        );
        assert_eq!(completed_through(at(10, 30), &Venue::Nse), today);
        assert_eq!(
            completed_through(at(12, 0), &Venue::Mcx),
            today.pred_opt().unwrap()
        );
    }

    #[test]
    fn the_instruments_csv_parses_and_indexes_by_exchange_and_symbol() {
        let csv = "instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange\n\
                   408065,1594,INFY,\"INFOSYS, LTD\",0,,0,0.05,1,EQ,NSE,NSE\n";
        let rows = parse_instruments(csv).unwrap();
        assert_eq!(rows[0].name, "INFOSYS, LTD");
        let index = token_index(&rows);
        assert_eq!(
            index.get(&("NSE".to_owned(), "INFY".to_owned())),
            Some(&"408065".to_owned())
        );
    }
}
