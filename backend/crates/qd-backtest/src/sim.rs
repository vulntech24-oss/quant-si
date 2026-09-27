//! Simulated clock and broker with conservative daily-bar fill rules (ADR 0006).
//!
//! An order becomes eligible on the first bar after the day it was placed
//! (decisions are made on completed bars). Fill rules, "acquire" meaning an
//! order that acquires units (OpenLong, CloseShort) and "dispose" the reverse:
//!
//! - Market: at the open, moved `slippage_ticks` against the order.
//! - Limit: acquire fills at the open if it is at or below the limit,
//!   otherwise at the limit if the low reaches it; dispose mirrors.
//! - Stop-market: acquire triggers at the open if it gaps through the
//!   trigger, otherwise at the trigger if the high reaches it; filled with
//!   adverse slippage. Dispose mirrors. Gaps fill at the open, never at the trigger.
//! - Stop-limit (entries): triggered as above, filled at the trigger, or at the
//!   open on a gap only if the open is within the limit.
//! - Day orders that do not fill on their first eligible bar expire.
//! - When a stop and a target of one OCO group could both fill on a bar, the
//!   stop fills (the conservative assumption) and the target is cancelled.
//! - A protective stop placed on the entry bar is checked against that bar's
//!   low (or high): if the bar reached it, the position is stopped out at the
//!   stop with slippage.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use qd_app::orders::{BrokerOrderRequest, OrderTerms};
use qd_app::ports::{
    BrokerAccountReader, BrokerError, BrokerFill, BrokerOrderAck, BrokerOrderExecutor,
    BrokerOrderId, BrokerPosition, Clock,
};
use qd_domain::action::{TradeAction, Transfer};
use qd_domain::ids::{InstrumentId, OrderIntentId};
use qd_domain::instrument::{InstrumentSpec, OrderType, Rounding, Validity};
use qd_domain::market::Bar;
use qd_domain::num::Price;
use rust_decimal::Decimal;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A clock the backtest moves forward.
#[derive(Debug)]
pub struct SimClock {
    now: Mutex<DateTime<Utc>>,
}

impl SimClock {
    /// Starts at `at`.
    #[must_use]
    pub const fn new(at: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(at),
        }
    }

    /// Moves the clock.
    pub fn set(&self, at: DateTime<Utc>) {
        *lock(&self.now) = at;
    }
}

impl Clock for SimClock {
    fn now(&self) -> DateTime<Utc> {
        *lock(&self.now)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SimStatus {
    Working,
    Filled,
    Cancelled,
    Expired,
}

#[derive(Clone, Debug)]
struct SimOrder {
    request: BrokerOrderRequest,
    placed_on: NaiveDate,
    status: SimStatus,
    sequence: u64,
}

/// What a bar did to the working orders.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BarEvents {
    /// Fills, in order.
    pub fills: Vec<BrokerFill>,
    /// Day orders that expired unfilled.
    pub expired: Vec<OrderIntentId>,
}

/// The simulated broker. Implements the order-executor port for the gateway
/// and the account-reader port for reconciliation.
pub struct SimBroker {
    clock: std::sync::Arc<dyn Clock>,
    specs: HashMap<InstrumentId, InstrumentSpec>,
    slippage_ticks: Decimal,
    orders: Mutex<HashMap<OrderIntentId, SimOrder>>,
    positions: Mutex<HashMap<InstrumentId, Decimal>>,
    sequence: Mutex<u64>,
}

impl std::fmt::Debug for SimBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimBroker")
            .field("slippage_ticks", &self.slippage_ticks)
            .finish_non_exhaustive()
    }
}

fn transfer(action: TradeAction) -> Transfer {
    action.transfer()
}

/// Adverse price after slippage, rounded to a tick against the order.
fn slipped(
    spec: &InstrumentSpec,
    price: Decimal,
    t: Transfer,
    slippage_ticks: Decimal,
) -> Option<Price> {
    let slip = slippage_ticks * spec.tick_size;
    let (raw, direction) = match t {
        Transfer::Purchase => (price + slip, Rounding::Up),
        Transfer::Disposal => (price - slip, Rounding::Down),
    };
    spec.round_price(raw, direction)
        .ok()
        .or_else(|| Price::new(spec.tick_size).ok())
}

/// The fill price of one order on one bar under the module's rules, if it
/// fills. `same_bar` checks only the part of the entry bar after the entry
/// (protective stops only). Pure, so the rules are testable on their own.
#[must_use]
pub fn simulate_fill(
    spec: &InstrumentSpec,
    action: TradeAction,
    terms: OrderTerms,
    bar: &Bar,
    same_bar: bool,
    slippage_ticks: Decimal,
) -> Option<Price> {
    let t = transfer(action);
    let (open, high, low) = (bar.open().value(), bar.high().value(), bar.low().value());
    let slip = |price: Decimal| slipped(spec, price, t, slippage_ticks);
    match terms.order_type {
        OrderType::Market if !same_bar => slip(open),
        OrderType::Limit if !same_bar => {
            let limit = terms.limit?.value();
            match t {
                Transfer::Purchase if open <= limit => Price::new(open).ok(),
                Transfer::Purchase if low <= limit => Price::new(limit).ok(),
                Transfer::Disposal if open >= limit => Price::new(open).ok(),
                Transfer::Disposal if high >= limit => Price::new(limit).ok(),
                _ => None,
            }
        }
        OrderType::StopMarket => {
            let trigger = terms.trigger?.value();
            match t {
                Transfer::Purchase if !same_bar && open >= trigger => slip(open),
                Transfer::Purchase if high >= trigger => slip(trigger),
                Transfer::Disposal if !same_bar && open <= trigger => slip(open),
                Transfer::Disposal if low <= trigger => slip(trigger),
                _ => None,
            }
        }
        OrderType::StopLimit if !same_bar => {
            let trigger = terms.trigger?.value();
            let limit = terms.limit?.value();
            match t {
                Transfer::Purchase if open >= trigger => {
                    (open <= limit).then(|| Price::new(open).ok()).flatten()
                }
                Transfer::Purchase if high >= trigger => Price::new(trigger).ok(),
                Transfer::Disposal if open <= trigger => {
                    (open >= limit).then(|| Price::new(open).ok()).flatten()
                }
                Transfer::Disposal if low <= trigger => Price::new(trigger).ok(),
                _ => None,
            }
        }
        _ => None,
    }
}

impl SimBroker {
    /// Creates a broker for these instruments with `slippage_ticks` of adverse
    /// slippage on market and stop fills.
    #[must_use]
    pub fn new(
        clock: std::sync::Arc<dyn Clock>,
        specs: Vec<InstrumentSpec>,
        slippage_ticks: Decimal,
    ) -> Self {
        Self {
            clock,
            specs: specs.into_iter().map(|s| (s.id, s)).collect(),
            slippage_ticks,
            orders: Mutex::new(HashMap::new()),
            positions: Mutex::new(HashMap::new()),
            sequence: Mutex::new(0),
        }
    }

    fn fill_price(&self, order: &SimOrder, bar: &Bar, same_bar: bool) -> Option<Price> {
        let spec = self.specs.get(&order.request.instrument())?;
        simulate_fill(
            spec,
            order.request.action(),
            order.request.terms(),
            bar,
            same_bar,
            self.slippage_ticks,
        )
    }

    fn priority(order: &SimOrder) -> u8 {
        match order.request.terms().order_type {
            OrderType::StopMarket => 0,
            OrderType::Market => 1,
            OrderType::StopLimit => 2,
            OrderType::Limit => 3,
        }
    }

    fn apply_fills(&self, instrument: InstrumentId, bar: &Bar, same_bar: bool) -> BarEvents {
        let at = self.clock.now();
        let mut orders = lock(&self.orders);
        let mut candidates: Vec<SimOrder> = orders
            .values()
            .filter(|o| {
                o.status == SimStatus::Working
                    && o.request.instrument() == instrument
                    && if same_bar {
                        o.placed_on == bar.date()
                            && o.request.terms().order_type == OrderType::StopMarket
                    } else {
                        o.placed_on < bar.date()
                    }
            })
            .cloned()
            .collect();
        candidates.sort_by_key(|o| (Self::priority(o), o.sequence));
        let mut events = BarEvents::default();
        let mut filled_groups = Vec::new();
        for order in candidates {
            let id = order.request.client_order_id();
            if order
                .request
                .oco_group()
                .is_some_and(|g| filled_groups.contains(&g))
            {
                if let Some(o) = orders.get_mut(&id) {
                    o.status = SimStatus::Cancelled;
                }
                continue;
            }
            match self.fill_price(&order, bar, same_bar) {
                Some(price) => {
                    if let Some(o) = orders.get_mut(&id) {
                        o.status = SimStatus::Filled;
                    }
                    if let Some(group) = order.request.oco_group() {
                        filled_groups.push(group);
                    }
                    let signed = match transfer(order.request.action()) {
                        Transfer::Purchase => order.request.quantity().value(),
                        Transfer::Disposal => -order.request.quantity().value(),
                    };
                    *lock(&self.positions).entry(instrument).or_default() += signed;
                    events.fills.push(BrokerFill {
                        client_order_id: id,
                        quantity: order.request.quantity(),
                        price,
                        at,
                    });
                }
                None if !same_bar && order.request.terms().validity == Validity::Day => {
                    if let Some(o) = orders.get_mut(&id) {
                        o.status = SimStatus::Expired;
                    }
                    events.expired.push(id);
                }
                None => {}
            }
        }
        events
    }

    /// Processes one completed bar for an instrument.
    pub fn process_bar(&self, instrument: InstrumentId, bar: &Bar) -> BarEvents {
        self.apply_fills(instrument, bar, false)
    }

    /// Checks protective stops placed on this bar against the rest of the bar.
    pub fn process_same_bar_protection(&self, instrument: InstrumentId, bar: &Bar) -> BarEvents {
        self.apply_fills(instrument, bar, true)
    }
}

#[async_trait]
impl BrokerOrderExecutor for SimBroker {
    async fn submit(&self, request: &BrokerOrderRequest) -> Result<BrokerOrderAck, BrokerError> {
        if !self.specs.contains_key(&request.instrument()) {
            return Err(BrokerError::Rejected("unknown instrument".to_owned()));
        }
        let id = request.client_order_id();
        let mut orders = lock(&self.orders);
        // Idempotent on the client order id: a resubmission changes nothing.
        orders.entry(id).or_insert_with(|| {
            let mut s = lock(&self.sequence);
            *s += 1;
            SimOrder {
                request: request.clone(),
                placed_on: self.clock.now().date_naive(),
                status: SimStatus::Working,
                sequence: *s,
            }
        });
        Ok(BrokerOrderAck {
            broker_order_id: BrokerOrderId(format!("SIM-{id}")),
        })
    }

    async fn cancel(&self, client_order_id: OrderIntentId) -> Result<(), BrokerError> {
        let mut orders = lock(&self.orders);
        let order = orders
            .get_mut(&client_order_id)
            .ok_or_else(|| BrokerError::Rejected("unknown order".to_owned()))?;
        match order.status {
            SimStatus::Working | SimStatus::Cancelled => {
                order.status = SimStatus::Cancelled;
                Ok(())
            }
            SimStatus::Filled => Err(BrokerError::Rejected("order already filled".to_owned())),
            SimStatus::Expired => Err(BrokerError::Rejected("order expired".to_owned())),
        }
    }
}

#[async_trait]
impl BrokerAccountReader for SimBroker {
    async fn positions(&self) -> Result<Vec<BrokerPosition>, BrokerError> {
        Ok(lock(&self.positions)
            .iter()
            .filter(|(_, q)| !q.is_zero())
            .map(|(instrument, net)| BrokerPosition {
                instrument: *instrument,
                net_quantity: *net,
            })
            .collect())
    }
}
