//! Simulated clock and venue. Both live in the shared layers so that paper
//! trading uses exactly the same fill rules (INV-08); this module keeps the
//! backtest names.

pub use qd_app::session::{BarEvents, SessionClock as SimClock};
pub use qd_broker_paper::{PaperBroker as SimBroker, simulate_fill};
