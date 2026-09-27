//! AI prediction tracking (ADR 0016): prediction → horizon → realized
//! result → accuracy.
//!
//! Scoring is deterministic and uses completed daily bars only:
//!
//! - The horizon ends on the `horizon_days`-th bar after the reference date.
//! - With a target and a stop, the first level touched decides; a bar
//!   touching both counts as the stop (conservative).
//! - Without levels, the direction of the horizon close decides.
//!
//! The scorecard reports accuracy, the Brier score of the stated
//! probabilities, calibration by probability band, and the P&L of the
//! trades the predictions led to.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use qd_app::journal::{AiPrediction, AiPredictionOutcome, PredictedDirection};
use qd_app::session::TradeRecord;
use qd_domain::ids::PredictionId;
use qd_domain::market::Bar;
use rust_decimal::Decimal;
use serde::Serialize;

/// Scores a prediction once its horizon has passed; `None` while it is
/// still open. `bars` are completed daily bars, oldest first.
#[must_use]
pub fn evaluate(p: &AiPrediction, bars: &[Bar], at: DateTime<Utc>) -> Option<AiPredictionOutcome> {
    let after: Vec<&Bar> = bars
        .iter()
        .filter(|b| b.date() > p.reference_date)
        .collect();
    let horizon = usize::from(p.horizon_days.max(1));
    if p.reference_price <= Decimal::ZERO {
        return None;
    }
    let up = p.direction == PredictedDirection::Up;
    // Levels: the first touch decides, even before the horizon.
    let mut levels = None;
    let mut decided_on = None;
    if let (Some(target), Some(stop)) = (p.target_price, p.stop_price) {
        for bar in after.iter().take(horizon) {
            let (high, low) = (bar.high().value(), bar.low().value());
            let (hit_target, hit_stop) = if up {
                (high >= target, low <= stop)
            } else {
                (low <= target, high >= stop)
            };
            if hit_stop {
                levels = Some("stop".to_owned());
                decided_on = Some(**bar);
                break;
            }
            if hit_target {
                levels = Some("target".to_owned());
                decided_on = Some(**bar);
                break;
            }
        }
        if levels.is_none() {
            if after.len() < horizon {
                return None;
            }
            levels = Some("neither".to_owned());
        }
    } else if after.len() < horizon {
        return None;
    }
    let end = decided_on.or_else(|| after.get(horizon - 1).map(|b| **b))?;
    let end_price = end.close().value();
    let return_pct = (end_price / p.reference_price - Decimal::ONE).round_dp(6);
    let direction_correct = if up {
        end_price > p.reference_price
    } else {
        end_price < p.reference_price
    };
    let correct = match levels.as_deref() {
        Some("target") => true,
        Some("stop") => false,
        _ => direction_correct,
    };
    Some(AiPredictionOutcome {
        prediction: p.id,
        as_of: end.date(),
        end_price,
        return_pct,
        direction_correct,
        levels,
        correct,
        at,
    })
}

/// One probability band of the calibration table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Band {
    /// Lower bound (inclusive).
    pub from: Decimal,
    /// Upper bound (exclusive; 1 is included in the last band).
    pub to: Decimal,
    /// Scored predictions in the band.
    pub count: u32,
    /// Mean stated probability.
    pub mean_probability: Decimal,
    /// Share that came true.
    pub hit_rate: Decimal,
}

/// The AI's track record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Scorecard {
    /// Predictions recorded.
    pub predictions: u32,
    /// Predictions scored.
    pub evaluated: u32,
    /// Scored predictions that came true.
    pub correct: u32,
    /// `correct / evaluated`.
    pub accuracy: Option<Decimal>,
    /// Scored predictions whose direction was right at the horizon.
    pub direction_correct: u32,
    /// Mean squared error of the stated probabilities (0 is perfect, 0.25 is a coin flip).
    pub brier: Option<Decimal>,
    /// Calibration by stated probability.
    pub bands: Vec<Band>,
    /// Closed trades the predictions led to.
    pub trades: u32,
    /// Of those, with a positive net P&L.
    pub winning_trades: u32,
    /// Net P&L of those trades.
    pub trade_net_pnl: Decimal,
    /// Mean R of those trades.
    pub trade_mean_r: Option<Decimal>,
}

/// Builds the scorecard. `trades` are closed trades from the journal; a
/// trade counts when its decision is one a prediction traded under.
#[must_use]
pub fn scorecard(
    predictions: &[AiPrediction],
    outcomes: &[AiPredictionOutcome],
    trades: &[TradeRecord],
) -> Scorecard {
    let by_id: HashMap<PredictionId, &AiPrediction> =
        predictions.iter().map(|p| (p.id, p)).collect();
    let mut evaluated = 0_u32;
    let mut correct = 0_u32;
    let mut direction_correct = 0_u32;
    let mut squared = Decimal::ZERO;
    let bounds = [
        (Decimal::ZERO, Decimal::new(4, 1)),
        (Decimal::new(4, 1), Decimal::new(55, 2)),
        (Decimal::new(55, 2), Decimal::new(7, 1)),
        (Decimal::new(7, 1), Decimal::new(85, 2)),
        (Decimal::new(85, 2), Decimal::ONE),
    ];
    let mut bands: Vec<(u32, Decimal, u32)> = vec![(0, Decimal::ZERO, 0); bounds.len()];
    let mut seen = std::collections::HashSet::new();
    for o in outcomes {
        let Some(p) = by_id.get(&o.prediction) else {
            continue;
        };
        if !seen.insert(o.prediction) {
            continue;
        }
        evaluated += 1;
        let hit = if o.correct {
            Decimal::ONE
        } else {
            Decimal::ZERO
        };
        if o.correct {
            correct += 1;
        }
        if o.direction_correct {
            direction_correct += 1;
        }
        let diff = p.probability - hit;
        squared += diff * diff;
        let last = bounds.len() - 1;
        let band = bounds
            .iter()
            .position(|(from, to)| p.probability >= *from && p.probability < *to)
            .unwrap_or(last);
        if let Some(b) = bands.get_mut(band) {
            b.0 += 1;
            b.1 += p.probability;
            if o.correct {
                b.2 += 1;
            }
        }
    }
    let ratio = |n: u32, d: u32| (d > 0).then(|| (Decimal::from(n) / Decimal::from(d)).round_dp(4));
    let decisions: std::collections::HashSet<_> =
        predictions.iter().filter_map(|p| p.decision).collect();
    let mut trade_count = 0_u32;
    let mut wins = 0_u32;
    let mut net = Decimal::ZERO;
    let mut r = Decimal::ZERO;
    for t in trades {
        if t.decision.is_some_and(|d| decisions.contains(&d)) {
            trade_count += 1;
            if t.net_pnl > Decimal::ZERO {
                wins += 1;
            }
            net += t.net_pnl;
            r += t.r_multiple;
        }
    }
    Scorecard {
        predictions: u32::try_from(predictions.len()).unwrap_or(u32::MAX),
        evaluated,
        correct,
        accuracy: ratio(correct, evaluated),
        direction_correct,
        brier: (evaluated > 0).then(|| (squared / Decimal::from(evaluated)).round_dp(4)),
        bands: bounds
            .iter()
            .zip(bands)
            .map(|((from, to), (count, sum, hits))| Band {
                from: *from,
                to: *to,
                count,
                mean_probability: if count == 0 {
                    Decimal::ZERO
                } else {
                    (sum / Decimal::from(count)).round_dp(4)
                },
                hit_rate: ratio(hits, count).unwrap_or(Decimal::ZERO),
            })
            .collect(),
        trades: trade_count,
        winning_trades: wins,
        trade_net_pnl: net,
        trade_mean_r: (trade_count > 0).then(|| (r / Decimal::from(trade_count)).round_dp(4)),
    }
}
