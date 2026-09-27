//! Loading what a trading run needs, shared by the paper and live runners.
//!
//! - [`load_state`]: the account's trading state, replayed from the journal.
//! - [`stage_slots`]: registered versions at the given stages whose logic
//!   version and parameters match the code in this build (INV-10).
//! - [`load_instruments`]: the latest spec of every instrument effective on
//!   a date, with its bars as known at a moment (INV-09).

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use qd_domain::economics::SlippageAssumption;
use qd_domain::ids::{AccountId, InstrumentId};
use qd_domain::instrument::{InstrumentKind, InstrumentSpec, ProductType};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::{Bar, BarSeries};
use qd_strategy::catalog::CatalogEntry;
use rust_decimal::Decimal;

use crate::decision::StrategyVersionInfo;
use crate::ports::{HistoricalMarketData, JournalReader, StoreError};
use crate::registry::StrategyRegistry;
use crate::restore::{RestoreError, RestoredState, STATE_KINDS};
use crate::session::{InstrumentData, StrategySlot};

/// A restored book as JSON: last day, positions, working orders and any
/// inconsistency (the paper and live state endpoints).
#[must_use]
pub fn book_json(account: AccountId, state: &RestoredState) -> serde_json::Value {
    use qd_domain::lifecycle::order::OrderIntentState as S;
    let consistent = state.check().map_err(|e| e.to_string()).err();
    let orders: Vec<serde_json::Value> = state
        .intents
        .iter()
        .filter(|r| {
            matches!(
                r.state,
                S::PendingSubmit | S::Submitted | S::PartiallyFilled | S::Unknown
            )
        })
        .map(|r| {
            serde_json::json!({
                "intent": r.intent,
                "state": r.state,
                "filled": r.filled,
                "broker_order_id": r.broker_order_id,
            })
        })
        .collect();
    serde_json::json!({
        "account": account,
        "last_day": state.last_day,
        "positions": state.positions,
        "working_orders": orders,
        "inconsistency": consistent,
    })
}

/// Journal entries read per page when restoring.
const PAGE: i64 = 1000;

/// Why loading failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    /// A store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The journal could not be replayed.
    #[error("restore failed: {0}")]
    Restore(RestoreError),
    /// Invalid data.
    #[error("invalid: {0}")]
    Invalid(String),
}

/// The product a run trades an instrument with.
#[must_use]
pub fn product_for(spec: &InstrumentSpec) -> ProductType {
    if spec.kind == InstrumentKind::Future {
        ProductType::Margin
    } else {
        ProductType::Delivery
    }
}

/// Rebuilds the account's trading state from the journal (not checked).
pub async fn load_state(
    reader: &dyn JournalReader,
    account: AccountId,
) -> Result<RestoredState, LoadError> {
    let mut values = Vec::new();
    let mut after = 0;
    loop {
        let page = reader.replay(&STATE_KINDS, after, PAGE).await?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        let full = i64::try_from(page.len()).unwrap_or(0) >= PAGE;
        values.extend(page.into_iter().map(|e| e.entry));
        if !full {
            break;
        }
    }
    RestoredState::from_entries(account, values.iter()).map_err(LoadError::Restore)
}

/// Strategy slots for every registered version at one of `stages` whose
/// logic version is in `catalog` with the registered parameters. Returns
/// the slots and, for versions at those stages that cannot run, why.
pub async fn stage_slots<'a>(
    registry: &StrategyRegistry,
    catalog: &'a [CatalogEntry],
    stages: &[StrategyStage],
    slippage_ticks: Decimal,
) -> Result<(Vec<StrategySlot<'a>>, Vec<String>), LoadError> {
    let store = |e: crate::registry::RegistryError| LoadError::Store(StoreError(e.to_string()));
    let mut slots = Vec::new();
    let mut skipped = Vec::new();
    for v in registry.versions().await.map_err(store)? {
        let stage = registry
            .stage(v.reference.version_id)
            .await
            .map_err(store)?;
        if !stages.contains(&stage) {
            continue;
        }
        let label = format!("{} v{}", v.reference.name, v.reference.version_number);
        let Some(entry) = catalog
            .iter()
            .find(|c| c.strategy.logic_version() == v.reference.logic_version)
        else {
            skipped.push(format!("{label}: logic version not in this build"));
            continue;
        };
        if entry.parameters != v.parameters {
            skipped.push(format!(
                "{label}: registered parameters differ from the logic version's"
            ));
            continue;
        }
        let slippage = SlippageAssumption::new("slip-v1", Decimal::ZERO)
            .map_err(|e| LoadError::Invalid(e.to_string()))?;
        slots.push(StrategySlot {
            strategy: entry.strategy.as_ref(),
            info: StrategyVersionInfo {
                reference: v.reference.clone(),
                stage,
                rr_floor: v.rr_floor,
                slippage,
            },
            slippage_ticks: Some(slippage_ticks),
        });
    }
    Ok((slots, skipped))
}

/// The latest spec of every instrument effective on `through`, and the
/// bars from `from` through `through` as known at `known_at` for those
/// that have any.
pub async fn load_instruments(
    market: &dyn HistoricalMarketData,
    from: NaiveDate,
    through: NaiveDate,
    known_at: DateTime<Utc>,
) -> Result<(Vec<InstrumentSpec>, Vec<InstrumentData>), LoadError> {
    let mut latest: HashMap<InstrumentId, InstrumentSpec> = HashMap::new();
    for spec in market.instruments(through).await? {
        let newer = latest
            .get(&spec.id)
            .is_none_or(|s| spec.version > s.version);
        if newer {
            latest.insert(spec.id, spec);
        }
    }
    let mut specs: Vec<InstrumentSpec> = latest.into_values().collect();
    specs.sort_by_key(|s| s.id);
    let mut data = Vec::new();
    for spec in &specs {
        let bars = market.daily_bars(spec.id, from, through, known_at).await?;
        let Some(last) = bars.last().map(Bar::date) else {
            continue;
        };
        let series =
            BarSeries::new(spec.id, last, bars).map_err(|e| LoadError::Invalid(e.to_string()))?;
        data.push(InstrumentData {
            spec: spec.clone(),
            product: product_for(spec),
            series,
        });
    }
    Ok((specs, data))
}
