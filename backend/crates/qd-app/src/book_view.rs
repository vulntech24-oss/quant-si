//! The portfolio view of a book (paper or live, ADR 0015): equity curve and
//! drawdown from the journaled days, exposure of open positions by
//! correlation bucket and asset class, and P&L per strategy from the booked
//! trades. Read-only; everything comes from the journal (ADR 0009).

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use qd_domain::ids::{DecisionId, InstrumentId};
use qd_domain::instrument::InstrumentSpec;
use rust_decimal::Decimal;
use serde::Serialize;

use crate::positions::Position;
use crate::session::DayRecord;

/// One point of the equity curve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EquityPoint {
    /// Trading date.
    pub date: NaiveDate,
    /// Equity at the close.
    pub equity: Decimal,
    /// Fall from the running peak, as a fraction (0 or negative).
    pub drawdown: Decimal,
}

/// Open exposure in one group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Exposure {
    /// Correlation bucket.
    pub bucket: String,
    /// Asset class.
    pub asset_class: String,
    /// Open positions.
    pub positions: u32,
    /// Quantity × entry price × multiplier, in the instrument currency.
    pub notional: Decimal,
    /// Loss to the stops, in the instrument currency.
    pub open_risk: Decimal,
}

/// Booked results of one strategy version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StrategyPnl {
    /// Strategy name and version, e.g. `Breakout v1`.
    pub strategy: String,
    /// Closed trades.
    pub trades: u32,
    /// Trades with a positive net P&L.
    pub wins: u32,
    /// Net P&L after costs.
    pub net_pnl: Decimal,
    /// Mean R multiple.
    pub mean_r: Decimal,
}

/// The whole view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BookView {
    /// Equity by day, oldest first.
    pub equity_curve: Vec<EquityPoint>,
    /// Deepest drawdown so far.
    pub max_drawdown: Decimal,
    /// Drawdown at the last day.
    pub current_drawdown: Decimal,
    /// Open exposure, largest notional first.
    pub exposure: Vec<Exposure>,
    /// Results per strategy, best first.
    pub strategies: Vec<StrategyPnl>,
}

/// Builds the view. `strategy_of` names the strategy behind each decision
/// that opened a trade; trades without one are grouped as "unknown".
#[must_use]
pub fn book_view(
    days: &[DayRecord],
    active: &[Position],
    specs: &HashMap<InstrumentId, InstrumentSpec>,
    strategy_of: &HashMap<DecisionId, String>,
) -> BookView {
    let mut peak = Decimal::ZERO;
    let mut max_drawdown = Decimal::ZERO;
    let mut equity_curve = Vec::with_capacity(days.len());
    for d in days {
        peak = peak.max(d.equity);
        let drawdown = if peak > Decimal::ZERO {
            (d.equity / peak - Decimal::ONE).round_dp(6)
        } else {
            Decimal::ZERO
        };
        max_drawdown = max_drawdown.min(drawdown);
        equity_curve.push(EquityPoint {
            date: d.date,
            equity: d.equity,
            drawdown,
        });
    }
    let current_drawdown = equity_curve.last().map_or(Decimal::ZERO, |p| p.drawdown);

    let mut groups: BTreeMap<(String, String), Exposure> = BTreeMap::new();
    for p in active {
        let (bucket, class) = specs.get(&p.instrument).map_or_else(
            || ("unknown".to_owned(), "unknown".to_owned()),
            |s| {
                (
                    s.correlation_bucket.0.clone(),
                    serde_json::to_value(s.asset_class)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "unknown".to_owned()),
                )
            },
        );
        let entry = p.entry_price.map_or(Decimal::ZERO, |e| e.value());
        let qty = p.quantity.value();
        let g = groups
            .entry((bucket.clone(), class.clone()))
            .or_insert(Exposure {
                bucket,
                asset_class: class,
                positions: 0,
                notional: Decimal::ZERO,
                open_risk: Decimal::ZERO,
            });
        g.positions = g.positions.saturating_add(1);
        g.notional += qty * entry * p.multiplier;
        if !entry.is_zero() {
            g.open_risk += (qty * (entry - p.stop.value()) * p.multiplier).abs();
        }
    }
    let mut exposure: Vec<Exposure> = groups.into_values().collect();
    exposure.sort_by_key(|e| std::cmp::Reverse(e.notional));

    let mut per: BTreeMap<String, (u32, u32, Decimal, Decimal)> = BTreeMap::new();
    for t in days.iter().flat_map(|d| &d.trades) {
        let name = t
            .decision
            .and_then(|d| strategy_of.get(&d))
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned());
        let e = per.entry(name).or_default();
        e.0 += 1;
        if t.net_pnl > Decimal::ZERO {
            e.1 += 1;
        }
        e.2 += t.net_pnl;
        e.3 += t.r_multiple;
    }
    let mut strategies: Vec<StrategyPnl> = per
        .into_iter()
        .map(|(strategy, (trades, wins, net_pnl, r))| StrategyPnl {
            strategy,
            trades,
            wins,
            net_pnl,
            mean_r: if trades == 0 {
                Decimal::ZERO
            } else {
                (r / Decimal::from(trades)).round_dp(4)
            },
        })
        .collect();
    strategies.sort_by_key(|s| std::cmp::Reverse(s.net_pnl));
    BookView {
        equity_curve,
        max_drawdown,
        current_drawdown,
        exposure,
        strategies,
    }
}

/// The portfolio view of each configured book, read from the journal.
pub struct JournalPortfolio {
    /// Journal reads.
    pub reader: std::sync::Arc<dyn crate::ports::JournalReader>,
    /// Instrument specs.
    pub market: std::sync::Arc<dyn crate::ports::HistoricalMarketData>,
    /// Clock.
    pub clock: std::sync::Arc<dyn crate::ports::Clock>,
    /// Book name (`paper`, `live`) and its account.
    pub books: Vec<(String, qd_domain::ids::AccountId)>,
}

impl std::fmt::Debug for JournalPortfolio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalPortfolio")
            .field("books", &self.books)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl crate::ports::PortfolioReader for JournalPortfolio {
    async fn view(&self, book: &str) -> Result<serde_json::Value, crate::ports::StoreError> {
        use crate::ports::StoreError;
        let account = self
            .books
            .iter()
            .find(|(name, _)| name == book)
            .map(|(_, a)| *a)
            .ok_or_else(|| StoreError(format!("no {book} book is configured")))?;
        let mut days = Vec::new();
        let mut after = 0;
        loop {
            let page = self.reader.replay(&["day_closed"], after, 1000).await?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            let full = page.len() >= 1000;
            days.extend(
                page.into_iter()
                    .filter_map(|e| serde_json::from_value::<DayRecord>(e.entry).ok())
                    .filter(|d| d.account == account),
            );
            if !full {
                break;
            }
        }
        let state = crate::runs::load_state(self.reader.as_ref(), account)
            .await
            .map_err(|e| StoreError(e.to_string()))?;
        let active = state.active_positions();
        let mut specs: HashMap<InstrumentId, InstrumentSpec> = HashMap::new();
        for s in self
            .market
            .instruments(self.clock.now().date_naive())
            .await?
        {
            if specs.get(&s.id).is_none_or(|old| s.version > old.version) {
                specs.insert(s.id, s);
            }
        }
        let mut strategy_of = HashMap::new();
        for id in days
            .iter()
            .flat_map(|d| &d.trades)
            .filter_map(|t| t.decision)
        {
            if strategy_of.contains_key(&id) {
                continue;
            }
            if let Some(entry) = self.reader.decision(id).await? {
                let name = entry
                    .entry
                    .pointer("/strategy/name")
                    .and_then(|v| v.as_str());
                let number = entry
                    .entry
                    .pointer("/strategy/version_number")
                    .and_then(serde_json::Value::as_u64);
                if let (Some(name), Some(number)) = (name, number) {
                    strategy_of.insert(id, format!("{name} v{number}"));
                }
            }
        }
        serde_json::to_value(book_view(&days, &active, &specs, &strategy_of))
            .map_err(|e| StoreError(e.to_string()))
    }
}
