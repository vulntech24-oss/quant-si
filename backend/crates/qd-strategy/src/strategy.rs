//! The `Strategy` trait and the shared evaluation pipeline (INV-08, INV-09, INV-10).
//!
//! A strategy turns features and a regime into one of three outputs: inactive
//! in this regime, no setup, or a setup candidate with a raw plan and its
//! explanation. It never sizes a position; probabilities, costs and the
//! complete proposal come from the Trade Proposal Engine, and the quantity from
//! the Risk Gate.

use qd_domain::instrument::InstrumentSpec;
use qd_domain::market::BarSeries;
use qd_domain::plan::TradePlanInput;
use qd_domain::proposal::{Grade, Reason};

use crate::features::{FeatureError, FeatureSet};
use crate::regime::{Regime, RegimeClassifier};

/// What a strategy sees. Only completed bars, only up to the decision date.
#[derive(Clone, Copy, Debug)]
pub struct StrategyInput<'a> {
    /// The instrument.
    pub spec: &'a InstrumentSpec,
    /// Completed bars.
    pub series: &'a BarSeries,
    /// Features at the last completed bar.
    pub features: &'a FeatureSet,
    /// Regime at the last completed bar.
    pub regime: Regime,
}

/// A setup the strategy wants to trade, before costs, probabilities and sizing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupCandidate {
    /// Raw plan levels; the proposal engine rounds them to ticks.
    pub plan: TradePlanInput,
    /// Setup type.
    pub setup_type: String,
    /// Grade.
    pub grade: Grade,
    /// Structured reasons.
    pub reasons: Vec<Reason>,
    /// The strongest argument against the trade.
    pub strongest_argument_against: String,
}

/// A strategy's output for one instrument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StrategyOutput {
    /// The strategy does not trade in this regime.
    Inactive {
        /// The regime.
        regime: Regime,
    },
    /// Active, but no setup today. Recorded compactly by the scanner, not as a decision.
    NoSetup,
    /// A setup candidate.
    Setup(Box<SetupCandidate>),
}

/// A strategy. Implementations are immutable: their parameters are fixed at
/// construction and identified by the logic version (INV-10).
pub trait Strategy: Send + Sync {
    /// Strategy name.
    fn name(&self) -> &str;

    /// Logic version. Any change to logic or parameters is a new version.
    fn logic_version(&self) -> &str;

    /// Evaluates one instrument.
    fn evaluate(&self, input: &StrategyInput<'_>) -> StrategyOutput;
}

/// Features, regime and output of one evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluation {
    /// Features used.
    pub features: FeatureSet,
    /// Regime used.
    pub regime: Regime,
    /// The strategy's output.
    pub output: StrategyOutput,
}

/// The one pipeline every mode uses: features → regime → strategy.
pub fn run_strategy(
    strategy: &dyn Strategy,
    spec: &InstrumentSpec,
    series: &BarSeries,
    classifier: &RegimeClassifier,
) -> Result<Evaluation, FeatureError> {
    let features = FeatureSet::compute(series)?;
    let regime = classifier.classify(&features);
    let output = strategy.evaluate(&StrategyInput {
        spec,
        series,
        features: &features,
        regime,
    });
    Ok(Evaluation {
        features,
        regime,
        output,
    })
}
