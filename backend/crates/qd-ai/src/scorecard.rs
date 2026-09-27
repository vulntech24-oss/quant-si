//! How each advisor's stances matched what happened (shadow-mode scoring).
//!
//! For every advised decision whose trade has closed: an `agree` is right
//! when the trade made money, and a `caution` or `disagree` is right when it
//! lost. The scorecard also compares mean R when the advisor agreed with
//! mean R when it did not. That comparison is what would justify ever
//! giving an advisor more weight, which INV-04 forbids without an ADR.

use std::collections::{BTreeMap, HashMap};

use qd_app::journal::{AiAdvice, AiStance};
use qd_app::session::TradeRecord;
use qd_domain::ids::DecisionId;
use rust_decimal::Decimal;
use serde::Serialize;

/// One advisor's record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AdvisorScore {
    /// Advisor.
    pub advisor: String,
    /// Decisions advised.
    pub advised: u32,
    /// Advised decisions whose trade has closed.
    pub scored: u32,
    /// Scored decisions where the stance matched the outcome.
    pub right: u32,
    /// Trades when the advisor agreed.
    pub agree_trades: u32,
    /// Mean R when it agreed.
    pub agree_mean_r: Decimal,
    /// Trades when it cautioned or disagreed.
    pub doubt_trades: u32,
    /// Mean R when it cautioned or disagreed.
    pub doubt_mean_r: Decimal,
}

fn mean(sum: Decimal, n: u32) -> Decimal {
    if n == 0 {
        Decimal::ZERO
    } else {
        (sum / Decimal::from(n)).round_dp(4)
    }
}

/// Scores every advisor. Abstentions are counted as advised, never scored.
#[must_use]
pub fn scorecard(advice: &[AiAdvice], trades: &[TradeRecord]) -> Vec<AdvisorScore> {
    let by_decision: HashMap<DecisionId, &TradeRecord> = trades
        .iter()
        .filter_map(|t| Some((t.decision?, t)))
        .collect();
    let mut scores: BTreeMap<&str, (AdvisorScore, Decimal, Decimal)> = BTreeMap::new();
    for a in advice {
        let (score, agree_sum, doubt_sum) = scores.entry(a.advisor.as_str()).or_insert_with(|| {
            (
                AdvisorScore {
                    advisor: a.advisor.clone(),
                    ..AdvisorScore::default()
                },
                Decimal::ZERO,
                Decimal::ZERO,
            )
        });
        score.advised += 1;
        let Some(t) = by_decision.get(&a.decision) else {
            continue;
        };
        let won = t.net_pnl > Decimal::ZERO;
        match a.stance {
            AiStance::Agree => {
                score.scored += 1;
                score.agree_trades += 1;
                *agree_sum += t.r_multiple;
                if won {
                    score.right += 1;
                }
            }
            AiStance::Caution | AiStance::Disagree => {
                score.scored += 1;
                score.doubt_trades += 1;
                *doubt_sum += t.r_multiple;
                if !won {
                    score.right += 1;
                }
            }
            AiStance::Abstain => {}
        }
    }
    scores
        .into_values()
        .map(|(mut s, agree_sum, doubt_sum)| {
            s.agree_mean_r = mean(agree_sum, s.agree_trades);
            s.doubt_mean_r = mean(doubt_sum, s.doubt_trades);
            s
        })
        .collect()
}
