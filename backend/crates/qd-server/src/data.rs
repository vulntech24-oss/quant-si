//! Instruments and bars from the web UI (ADR 0015): the same validation as
//! `qd instrument add` and `qd bars import`, plus the data-quality checks.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Duration;
use qd_app::ports::{AuditLog, DataAdmin, HistoricalMarketData, SettingsError, StoreError};
use qd_domain::calendar::{DataIssue, check_bars};
use qd_domain::ids::InstrumentId;
use qd_domain::instrument::{InstrumentSpec, InstrumentSpecData};
use qd_domain::market::{Bar, BarData, BarSeries};
use serde_json::{Value, json};

use crate::runtime::Runtime;

/// Most rows accepted in one upload.
pub const MAX_ROWS: usize = 20_000;

/// The data service over the runtime's stores.
pub struct DataService(pub Arc<Runtime>);

fn invalid(e: impl std::fmt::Display) -> SettingsError {
    SettingsError::Invalid(e.to_string())
}

impl DataService {
    async fn spec(&self, id: InstrumentId) -> Result<InstrumentSpec, StoreError> {
        self.0
            .stores
            .market
            .all_instruments()
            .await?
            .into_iter()
            .filter(|s| s.id == id)
            .max_by_key(|s| s.version)
            .ok_or_else(|| StoreError("unknown instrument".to_owned()))
    }
}

/// Parses and validates CSV bars: every row, then order and duplicates.
pub fn parse_bars(instrument: InstrumentId, csv_text: &str) -> Result<Vec<Bar>, String> {
    let mut reader = csv::Reader::from_reader(csv_text.as_bytes());
    let mut bars = Vec::new();
    for (i, record) in reader.deserialize::<BarData>().enumerate() {
        if i >= MAX_ROWS {
            return Err(format!("more than {MAX_ROWS} rows; split the file"));
        }
        let data = record.map_err(|e| format!("row {}: {e}", i + 2))?;
        bars.push(Bar::new(data).map_err(|e| format!("row {}: {e}", i + 2))?);
    }
    let last = bars.last().map(Bar::date).ok_or("no rows")?;
    let series = BarSeries::new(instrument, last, bars).map_err(|e| e.to_string())?;
    Ok(series.bars().to_vec())
}

#[async_trait]
impl DataAdmin for DataService {
    async fn add_instrument(&self, spec_toml: &str, actor: &str) -> Result<Value, SettingsError> {
        let data: InstrumentSpecData = toml::from_str(spec_toml).map_err(invalid)?;
        let spec = InstrumentSpec::new(data).map_err(invalid)?;
        let exists = self
            .0
            .stores
            .market
            .all_instruments()
            .await?
            .iter()
            .any(|s| s.id == spec.id && s.version == spec.version);
        if exists {
            return Err(SettingsError::Invalid(format!(
                "version {} of this instrument exists; specs are immutable, add a new version",
                spec.version
            )));
        }
        self.0.stores.market.add_instrument(&spec).await?;
        self.0
            .stores
            .audit
            .record(
                actor,
                "instrument.add",
                json!({ "id": spec.id, "version": spec.version, "symbol": spec.symbol }),
            )
            .await?;
        serde_json::to_value(&spec).map_err(invalid)
    }

    async fn import_bars(
        &self,
        instrument: InstrumentId,
        csv_text: &str,
        accept_jumps: bool,
        actor: &str,
    ) -> Result<Value, SettingsError> {
        let spec = self
            .spec(instrument)
            .await
            .map_err(|e| SettingsError::Invalid(e.0))?;
        let bars = parse_bars(instrument, csv_text).map_err(SettingsError::Invalid)?;
        let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
            return Err(SettingsError::Invalid("no rows".to_owned()));
        };
        let now = self.0.clock.now();
        let before = self
            .0
            .stores
            .market
            .daily_bars(
                instrument,
                first.date() - Duration::days(30),
                first.date() - Duration::days(1),
                now,
            )
            .await?;
        let limits = self.0.effective().await?.data.limits();
        let issues = check_bars(
            before.last(),
            &bars,
            self.0.config.calendars.get(&spec.calendar_id),
            limits,
        );
        let jumps = issues
            .iter()
            .filter(|i| matches!(i, DataIssue::PriceJump { .. }))
            .count();
        if jumps > 0 && !accept_jumps {
            return Err(SettingsError::Invalid(format!(
                "{jumps} suspect price jump(s) (first on {}); check for a split or bonus, then import again with \"accept jumps\"",
                issues
                    .iter()
                    .find(|i| matches!(i, DataIssue::PriceJump { .. }))
                    .map_or_else(String::new, |i| i.date().to_string())
            )));
        }
        let written = self
            .0
            .stores
            .market
            .insert_bars(instrument, &bars, now)
            .await?;
        self.0
            .stores
            .audit
            .record(
                actor,
                "bars.import",
                json!({
                    "instrument": instrument, "rows": written, "from": first.date(),
                    "to": last.date(), "accepted_jumps": if accept_jumps { jumps } else { 0 },
                }),
            )
            .await?;
        Ok(json!({ "rows_written": written, "issues": issues }))
    }

    async fn quality(&self, instrument: InstrumentId) -> Result<Value, StoreError> {
        let spec = self.spec(instrument).await?;
        let now = self.0.clock.now();
        let today = now.date_naive();
        let bars = self
            .0
            .stores
            .market
            .daily_bars(instrument, today - Duration::days(400), today, now)
            .await?;
        let limits = self.0.effective().await?.data.limits();
        let issues = check_bars(
            None,
            &bars,
            self.0.config.calendars.get(&spec.calendar_id),
            limits,
        );
        Ok(json!({
            "symbol": spec.symbol,
            "bars": bars.len(),
            "first": bars.first().map(Bar::date),
            "last": bars.last().map(Bar::date),
            "issues": issues,
        }))
    }
}
