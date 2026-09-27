//! Monte Carlo resampling of trade outcomes (ADR 0010).
//!
//! Bootstrap: each path draws `n` trades with replacement from the observed
//! R-multiples and compounds equity at a fixed fraction risked per trade.
//! The distribution of maximum drawdowns and final returns shows how much of
//! a backtest's result is luck of ordering. Deterministic for a seed, so a
//! validation report can be reproduced.
//!
//! Floats appear only inside these statistics (spec §6.1); inputs and
//! outputs are decimals.

use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};

/// Monte Carlo settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonteCarloConfig {
    /// Number of simulated paths.
    pub paths: u32,
    /// Random seed.
    pub seed: u64,
}

/// Distribution summaries of the simulated paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonteCarloReport {
    /// Paths simulated.
    pub paths: u32,
    /// Trades per path (the observed count).
    pub trades_per_path: u32,
    /// Fraction of equity risked per trade.
    pub risk_per_trade: Decimal,
    /// Median maximum drawdown, 0–1.
    pub max_drawdown_p50: Decimal,
    /// 95th percentile of the maximum drawdown.
    pub max_drawdown_p95: Decimal,
    /// 99th percentile of the maximum drawdown.
    pub max_drawdown_p99: Decimal,
    /// 5th percentile of the final return.
    pub final_return_p05: Decimal,
    /// Median final return.
    pub final_return_p50: Decimal,
    /// Share of paths whose drawdown reached `drawdown_limit`.
    pub prob_drawdown_over_limit: Decimal,
    /// The drawdown limit used (the hard-halt drawdown).
    pub drawdown_limit: Decimal,
}

/// SplitMix64: a small, well-mixed deterministic generator.
struct SplitMix64(u64);

impl SplitMix64 {
    const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform index in `0..n` (multiply-shift, negligible bias for small n).
    fn below(&mut self, n: usize) -> usize {
        let wide = u128::from(self.next()) * (n as u128);
        usize::try_from(wide >> 64).unwrap_or(0)
    }
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    #[allow(clippy::float_arithmetic, clippy::cast_precision_loss)] // statistics (spec §6.1)
    let rank = q * (sorted.len() - 1) as f64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // rank is within 0..len
    let index = rank.round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn dec(value: f64) -> Decimal {
    Decimal::from_f64(value).map_or(Decimal::ZERO, |d| d.round_dp(6))
}

/// Runs the bootstrap. `None` without trades or with an invalid risk fraction.
#[must_use]
#[allow(clippy::float_arithmetic)] // statistics (spec §6.1)
pub fn monte_carlo(
    r_multiples: &[Decimal],
    risk_per_trade: Decimal,
    drawdown_limit: Decimal,
    config: MonteCarloConfig,
) -> Option<MonteCarloReport> {
    let risk = risk_per_trade.to_f64().filter(|r| *r > 0.0 && *r < 1.0)?;
    let limit = drawdown_limit.to_f64()?;
    let rs: Vec<f64> = r_multiples.iter().filter_map(ToPrimitive::to_f64).collect();
    if rs.is_empty() || config.paths == 0 {
        return None;
    }
    let mut rng = SplitMix64(config.seed);
    let mut drawdowns = Vec::with_capacity(config.paths as usize);
    let mut finals = Vec::with_capacity(config.paths as usize);
    let mut over = 0_u32;
    for _ in 0..config.paths {
        let mut equity = 1.0_f64;
        let mut peak = 1.0_f64;
        let mut max_dd = 0.0_f64;
        for _ in 0..rs.len() {
            let r = rs[rng.below(rs.len())];
            // A loss can never take more than the whole account.
            equity = (equity * (1.0 + r * risk)).max(0.0);
            peak = peak.max(equity);
            if peak > 0.0 {
                max_dd = max_dd.max((peak - equity) / peak);
            }
        }
        if max_dd >= limit {
            over += 1;
        }
        drawdowns.push(max_dd);
        finals.push(equity - 1.0);
    }
    drawdowns.sort_by(f64::total_cmp);
    finals.sort_by(f64::total_cmp);
    Some(MonteCarloReport {
        paths: config.paths,
        trades_per_path: u32::try_from(rs.len()).unwrap_or(u32::MAX),
        risk_per_trade,
        max_drawdown_p50: dec(quantile(&drawdowns, 0.50)),
        max_drawdown_p95: dec(quantile(&drawdowns, 0.95)),
        max_drawdown_p99: dec(quantile(&drawdowns, 0.99)),
        final_return_p05: dec(quantile(&finals, 0.05)),
        final_return_p50: dec(quantile(&finals, 0.50)),
        prob_drawdown_over_limit: Decimal::from(over) / Decimal::from(config.paths),
        drawdown_limit,
    })
}
