//! Backtest metrics. Money stays Decimal; `f64` appears only in statistics
//! (Sharpe), as spec §6.1 allows.

use chrono::NaiveDate;
use qd_domain::action::Side;
use qd_domain::num::Quantity;
use qd_domain::outcome::ExitReason;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;

use crate::engine::EquityPoint;

/// One closed trade.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TradeRecord {
    /// Symbol.
    pub instrument: String,
    /// Side.
    pub side: Side,
    /// Entry fill date.
    pub opened_on: NaiveDate,
    /// Date the trade closed.
    pub closed_on: NaiveDate,
    /// Quantity entered.
    pub quantity: Quantity,
    /// Average entry price.
    pub entry_price: Decimal,
    /// Average exit price.
    pub exit_price: Decimal,
    /// P&L before costs.
    pub gross_pnl: Decimal,
    /// Round-trip costs from the cost model.
    pub costs: Decimal,
    /// P&L after costs.
    pub net_pnl: Decimal,
    /// Net P&L over the risk planned at entry.
    pub r_multiple: Decimal,
    /// Why it closed.
    pub exit_reason: ExitReason,
}

/// Summary statistics.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Metrics {
    /// Closed trades.
    pub trades: u32,
    /// Winning trades (net P&L > 0).
    pub wins: u32,
    /// Win rate, 0–1.
    pub win_rate: Decimal,
    /// Mean R-multiple (expectancy in R).
    pub expectancy_r: Decimal,
    /// Sum of net P&L.
    pub net_pnl: Decimal,
    /// Final equity over initial, minus 1.
    pub total_return: Decimal,
    /// Largest peak-to-trough fall of the equity curve, 0–1.
    pub max_drawdown: Decimal,
    /// Gross profit over gross loss; `None` without losses.
    pub profit_factor: Option<Decimal>,
    /// Annualized Sharpe ratio of daily equity returns (252 days), statistics only.
    pub sharpe: Option<f64>,
}

impl Metrics {
    /// Computes metrics from trades and the equity curve.
    #[must_use]
    pub fn compute(trades: &[TradeRecord], curve: &[EquityPoint], initial: Decimal) -> Self {
        let count = u32::try_from(trades.len()).unwrap_or(u32::MAX);
        let wins = u32::try_from(trades.iter().filter(|t| t.net_pnl > Decimal::ZERO).count())
            .unwrap_or(u32::MAX);
        let net_pnl: Decimal = trades.iter().map(|t| t.net_pnl).sum();
        let (win_rate, expectancy_r) = if count == 0 {
            (Decimal::ZERO, Decimal::ZERO)
        } else {
            let n = Decimal::from(count);
            (
                Decimal::from(wins) / n,
                trades.iter().map(|t| t.r_multiple).sum::<Decimal>() / n,
            )
        };
        let profit: Decimal = trades.iter().map(|t| t.net_pnl.max(Decimal::ZERO)).sum();
        let loss: Decimal = trades.iter().map(|t| (-t.net_pnl).max(Decimal::ZERO)).sum();
        let profit_factor = (!loss.is_zero()).then(|| profit / loss);
        let final_equity = curve.last().map_or(initial, |p| p.equity);
        let total_return = if initial.is_zero() {
            Decimal::ZERO
        } else {
            final_equity / initial - Decimal::ONE
        };
        let mut peak = initial;
        let mut max_drawdown = Decimal::ZERO;
        for point in curve {
            peak = peak.max(point.equity);
            if peak > Decimal::ZERO {
                max_drawdown = max_drawdown.max((peak - point.equity) / peak);
            }
        }
        Self {
            trades: count,
            wins,
            win_rate,
            expectancy_r,
            net_pnl,
            total_return,
            max_drawdown,
            profit_factor,
            sharpe: sharpe(curve),
        }
    }
}

/// Annualized Sharpe ratio of daily returns, zero risk-free rate. Statistics only.
#[allow(clippy::float_arithmetic)] // spec §6.1: floats are allowed inside statistics
fn sharpe(curve: &[EquityPoint]) -> Option<f64> {
    let values: Vec<f64> = curve.iter().filter_map(|p| p.equity.to_f64()).collect();
    let returns: Vec<f64> = values
        .windows(2)
        .filter_map(|w| match w {
            [a, b] if *a > 0.0 => Some(b / a - 1.0),
            _ => None,
        })
        .collect();
    if returns.len() < 2 {
        return None;
    }
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let sd = variance.sqrt();
    (sd > 0.0).then(|| mean / sd * 252_f64.sqrt())
}
