//! The Kite broker: order executor, account reader and the venue the daily
//! cycle reads fills from (`docs/integrations/kite.md`, "Mapping").
//!
//! - Orders are placed only in a build with the `live-orders` feature
//!   (INV-14); otherwise every placement is refused before any HTTP call.
//!   The Order Gateway is the only caller (INV-01).
//! - A protective stop and its target (one OCO group) become one two-leg GTT.
//!   Each leg is a LIMIT order; the stop leg's limit is the stop moved by the
//!   configured buffer against the exit, rounded to a tick.
//! - [`KiteBroker::refresh`] reads the order book and GTTs and queues fills
//!   and ended orders per instrument; the session drains them through
//!   [`SimulatedVenue::process_bar`].
//! - Kite errors after which nothing was done are rejections; transport
//!   failures and Kite-side failures leave the outcome unknown.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDateTime, NaiveTime, Utc, Weekday};
use qd_app::live::live_orders_compiled;
use qd_app::orders::{BrokerOrderRequest, OrderTerms};
use qd_app::ports::{
    BrokerAccountReader, BrokerError, BrokerFill, BrokerOrderAck, BrokerOrderExecutor,
    BrokerOrderId, BrokerPosition, Clock,
};
use qd_app::restore::RestoredState;
use qd_app::session::{BarEvents, SimulatedVenue};
use qd_domain::action::TradeAction;
use qd_domain::ids::{InstrumentId, OrderIntentId};
use qd_domain::instrument::{InstrumentSpec, OrderType, ProductType, Rounding, Validity, Venue};
use qd_domain::lifecycle::order::{OrderIntentState, ReconciledState};
use qd_domain::market::Bar;
use qd_domain::num::{Price, Quantity};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::{KiteClient, KiteError};
use crate::market::{KiteRef, decimal, ist, kite_ref};

/// Which Kite order variety to use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VarietyChoice {
    /// `regular` while the exchange is open, `amo` otherwise.
    #[default]
    Auto,
    /// Always `regular`.
    Regular,
    /// Always `amo`.
    Amo,
}

/// Order settings (Settings page, section `kite`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderSettings {
    /// `market_protection` on MARKET and SL-M orders: -1 (automatic) or 1–100.
    pub market_protection: i32,
    /// The stop leg's limit sits this fraction beyond the stop (0.01 = 1%).
    pub stop_limit_buffer: Decimal,
    /// Variety.
    pub variety: VarietyChoice,
}

impl OrderSettings {
    /// Range checks.
    pub fn validate(&self) -> Result<(), String> {
        if self.market_protection != -1 && !(1..=100).contains(&self.market_protection) {
            return Err("kite.market_protection must be -1 (automatic) or 1 to 100".to_owned());
        }
        if self.stop_limit_buffer <= Decimal::ZERO || self.stop_limit_buffer > Decimal::new(1, 1) {
            return Err("kite.stop_limit_buffer must be above 0 and at most 0.1".to_owned());
        }
        Ok(())
    }
}

/// Where an intent lives at Kite.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Placement {
    /// A regular or AMO order.
    Order(String),
    /// A leg of a GTT; once triggered, the order it placed.
    Gtt {
        trigger: String,
        leg: usize,
        order: Option<String>,
    },
}

impl Placement {
    fn broker_id(&self) -> String {
        match self {
            Self::Order(id) => id.clone(),
            Self::Gtt { trigger, leg, .. } => format!("gtt:{trigger}:{leg}"),
        }
    }

    fn parse(id: &str) -> Self {
        let mut parts = id.split(':');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("gtt"), Some(trigger), Some(leg), None) => leg.parse().map_or_else(
                |_| Self::Order(id.to_owned()),
                |leg| Self::Gtt {
                    trigger: trigger.to_owned(),
                    leg,
                    order: None,
                },
            ),
            _ => Self::Order(id.to_owned()),
        }
    }
}

#[derive(Clone, Debug)]
struct Tracked {
    instrument: InstrumentId,
    action: TradeAction,
    quantity: Decimal,
    reported: Decimal,
    placement: Placement,
}

#[derive(Default)]
struct State {
    tracked: HashMap<OrderIntentId, Tracked>,
    pending: HashMap<InstrumentId, BarEvents>,
}

/// One order leg, as the mapping sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leg {
    /// Intent id.
    pub id: OrderIntentId,
    /// Instrument.
    pub instrument: InstrumentId,
    /// Action.
    pub action: TradeAction,
    /// Quantity in units.
    pub quantity: Quantity,
    /// Terms.
    pub terms: OrderTerms,
}

impl From<&BrokerOrderRequest> for Leg {
    fn from(r: &BrokerOrderRequest) -> Self {
        Self {
            id: r.client_order_id(),
            instrument: r.instrument(),
            action: r.action(),
            quantity: r.quantity(),
            terms: r.terms(),
        }
    }
}

/// `BUY` or `SELL`.
#[must_use]
pub const fn transaction_type(action: TradeAction) -> &'static str {
    match action {
        TradeAction::OpenLong | TradeAction::CloseShort => "BUY",
        TradeAction::CloseLong | TradeAction::OpenShort => "SELL",
    }
}

/// The Kite `tag`: the last 20 hex digits of the intent id (its random part).
#[must_use]
pub fn tag(id: OrderIntentId) -> String {
    let simple = id.as_uuid().simple().to_string();
    simple[simple.len().saturating_sub(20)..].to_owned()
}

fn product(p: ProductType) -> Result<&'static str, String> {
    match p {
        ProductType::Delivery => Ok("CNC"),
        ProductType::Margin => Ok("NRML"),
        ProductType::Intraday => Ok("MIS"),
        ProductType::Spot => Err("spot products are not traded at Kite".to_owned()),
    }
}

fn units(q: Quantity) -> Result<String, String> {
    let v = q.value().normalize();
    if v.fract().is_zero() && v > Decimal::ZERO {
        Ok(v.to_string())
    } else {
        Err(format!("quantity {v} is not a whole number of units"))
    }
}

fn price_text(p: Price) -> String {
    p.value().normalize().to_string()
}

/// Whether the exchange's regular session is open at `now`.
#[must_use]
pub fn exchange_open(venue: &Venue, now: DateTime<Utc>) -> bool {
    let local = now.with_timezone(&ist());
    if matches!(local.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let (open, close) = match venue {
        Venue::Mcx => ((9, 0), (23, 30)),
        _ => ((9, 15), (15, 30)),
    };
    let at = |(h, m): (u32, u32)| NaiveTime::from_hms_opt(h, m, 0).unwrap_or(NaiveTime::MIN);
    local.time() >= at(open) && local.time() < at(close)
}

/// A form body: name and value pairs.
pub type Form = Vec<(&'static str, String)>;

/// The form of a regular or AMO order.
pub fn order_form(
    leg: &Leg,
    kref: &KiteRef,
    venue: &Venue,
    settings: &OrderSettings,
) -> Result<Form, String> {
    let terms = leg.terms;
    let mut form = vec![
        ("tradingsymbol", kref.tradingsymbol.clone()),
        ("exchange", kref.exchange.clone()),
        ("transaction_type", transaction_type(leg.action).to_owned()),
        ("quantity", units(leg.quantity)?),
        ("product", product(terms.product)?.to_owned()),
        ("tag", tag(leg.id)),
    ];
    let validity = match terms.validity {
        Validity::Day => "DAY",
        Validity::ImmediateOrCancel if *venue == Venue::Mcx => {
            return Err("MCX does not accept IOC orders from algorithms".to_owned());
        }
        Validity::ImmediateOrCancel => "IOC",
        Validity::GoodTillCancelled => {
            return Err("good-till-cancelled orders are placed as GTTs".to_owned());
        }
    };
    form.push(("validity", validity.to_owned()));
    let protection = settings.market_protection.to_string();
    let missing = || "prices do not match the order type".to_owned();
    match terms.order_type {
        OrderType::Market => {
            form.push(("order_type", "MARKET".to_owned()));
            form.push(("market_protection", protection));
        }
        OrderType::Limit => {
            form.push(("order_type", "LIMIT".to_owned()));
            form.push(("price", price_text(terms.limit.ok_or_else(missing)?)));
        }
        OrderType::StopLimit => {
            form.push(("order_type", "SL".to_owned()));
            form.push(("price", price_text(terms.limit.ok_or_else(missing)?)));
            form.push((
                "trigger_price",
                price_text(terms.trigger.ok_or_else(missing)?),
            ));
        }
        OrderType::StopMarket => {
            form.push(("order_type", "SL-M".to_owned()));
            form.push((
                "trigger_price",
                price_text(terms.trigger.ok_or_else(missing)?),
            ));
            form.push(("market_protection", protection));
        }
    }
    Ok(form)
}

/// A GTT leg's trigger value and LIMIT price.
fn gtt_leg(leg: &Leg, spec: &InstrumentSpec, buffer: Decimal) -> Result<(Price, Price), String> {
    let terms = leg.terms;
    let missing = || "prices do not match the order type".to_owned();
    match terms.order_type {
        OrderType::Limit => {
            let limit = terms.limit.ok_or_else(missing)?;
            Ok((limit, limit))
        }
        OrderType::StopLimit => Ok((
            terms.trigger.ok_or_else(missing)?,
            terms.limit.ok_or_else(missing)?,
        )),
        OrderType::StopMarket => {
            let trigger = terms.trigger.ok_or_else(missing)?;
            // The limit gives way against the exit, so the order still fills
            // after a move through the stop; rounded further away, not back.
            let (factor, direction) = match transaction_type(leg.action) {
                "SELL" => (Decimal::ONE - buffer, Rounding::Down),
                _ => (Decimal::ONE + buffer, Rounding::Up),
            };
            let limit = spec
                .round_price(trigger.value() * factor, direction)
                .map_err(|e| e.to_string())?;
            Ok((trigger, limit))
        }
        OrderType::Market => Err("market orders cannot be GTT legs".to_owned()),
    }
}

/// The form of a GTT for one leg (`single`) or an OCO pair (`two-leg`).
/// Returns the form and, per leg in the caller's order, its index in the GTT.
pub fn gtt_form(
    legs: &[Leg],
    kref: &KiteRef,
    spec: &InstrumentSpec,
    settings: &OrderSettings,
    last_price: Price,
) -> Result<(Form, Vec<usize>), String> {
    let first = legs.first().ok_or("no legs")?;
    if legs.len() > 2
        || legs.iter().any(|l| {
            l.instrument != first.instrument
                || transaction_type(l.action) != transaction_type(first.action)
                || l.quantity != first.quantity
                || l.terms.product != first.terms.product
        })
    {
        return Err(
            "GTT legs must be one or two exits of one instrument, side and size".to_owned(),
        );
    }
    let mut priced = Vec::with_capacity(legs.len());
    for (i, leg) in legs.iter().enumerate() {
        let (trigger, limit) = gtt_leg(leg, spec, settings.stop_limit_buffer)?;
        priced.push((i, trigger, limit));
    }
    priced.sort_by_key(|(_, trigger, _)| *trigger);
    let last = last_price.value();
    match priced.as_slice() {
        [(_, low, _), (_, high, _)] if !(low.value() < last && last < high.value()) => {
            return Err(format!(
                "last price {last} is not between the triggers {low} and {high}"
            ));
        }
        [(_, trigger, _)] if trigger.value() == last => {
            return Err(format!("last price {last} is at the trigger"));
        }
        _ => {}
    }
    let orders: Vec<Value> = priced
        .iter()
        .map(|(i, _, limit)| {
            Ok(json!({
                "exchange": kref.exchange,
                "tradingsymbol": kref.tradingsymbol,
                "transaction_type": transaction_type(legs[*i].action),
                "quantity": units(legs[*i].quantity)?.parse::<u64>().map_err(|e| e.to_string())?,
                "order_type": "LIMIT",
                "product": product(legs[*i].terms.product)?,
                "price": limit.value().normalize(),
            }))
        })
        .collect::<Result<_, String>>()?;
    let triggers: Vec<Decimal> = priced
        .iter()
        .map(|(_, t, _)| t.value().normalize())
        .collect();
    let condition = json!({
        "exchange": kref.exchange,
        "tradingsymbol": kref.tradingsymbol,
        "trigger_values": triggers,
        "last_price": last.normalize(),
    });
    let mut index = vec![0; legs.len()];
    for (position, (i, _, _)) in priced.iter().enumerate() {
        index[*i] = position;
    }
    let kind = if legs.len() == 2 { "two-leg" } else { "single" };
    Ok((
        vec![
            ("type", kind.to_owned()),
            ("condition", condition.to_string()),
            ("orders", Value::Array(orders).to_string()),
        ],
        index,
    ))
}

fn broker_error(e: KiteError) -> BrokerError {
    match e {
        KiteError::Token(_) | KiteError::Refused { .. } | KiteError::NotLoggedIn => {
            BrokerError::Rejected(e.to_string())
        }
        KiteError::Unavailable(_) | KiteError::Unexpected(_) => {
            BrokerError::Transport(e.to_string())
        }
    }
}

fn not_compiled() -> BrokerError {
    BrokerError::Rejected(
        "this build cannot place live orders (live-orders feature off)".to_owned(),
    )
}

/// A Kite order from the order book.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BookOrder {
    status: String,
    variety: String,
    filled: Decimal,
    average_price: Option<Decimal>,
    at: Option<DateTime<Utc>>,
    tag: Option<String>,
}

fn parse_book(data: &Value) -> HashMap<String, BookOrder> {
    let mut book = HashMap::new();
    for o in data.as_array().into_iter().flatten() {
        let Some(id) = o.get("order_id").and_then(Value::as_str) else {
            continue;
        };
        let text = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
        let at = text("exchange_update_timestamp")
            .or_else(|| text("order_timestamp"))
            .and_then(|t| NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M:%S").ok())
            .and_then(|t| t.and_local_timezone(ist()).single())
            .map(|t| t.with_timezone(&Utc));
        book.insert(
            id.to_owned(),
            BookOrder {
                status: text("status").unwrap_or_default(),
                variety: text("variety").unwrap_or_else(|| "regular".to_owned()),
                filled: o
                    .get("filled_quantity")
                    .and_then(decimal)
                    .unwrap_or_default(),
                average_price: o
                    .get("average_price")
                    .and_then(decimal)
                    .filter(|p| *p > Decimal::ZERO),
                at,
                tag: text("tag"),
            },
        );
    }
    book
}

/// A summary of one refresh.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RefreshReport {
    /// Fills queued.
    pub fills: usize,
    /// One line per fill, e.g. `BUY 10 NSE:INFY @ 1500.5 (entry)`.
    pub fill_notes: Vec<String>,
    /// Orders that ended without a (further) fill.
    pub ended: usize,
    /// Problems worth a human's attention.
    pub notes: Vec<String>,
}

/// The Kite broker for one account.
pub struct KiteBroker {
    client: KiteClient,
    settings: OrderSettings,
    clock: Arc<dyn Clock>,
    specs: HashMap<InstrumentId, (KiteRef, InstrumentSpec)>,
    by_symbol: HashMap<(String, String), InstrumentId>,
    state: Mutex<State>,
}

impl std::fmt::Debug for KiteBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiteBroker")
            .field("client", &self.client)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl KiteBroker {
    /// A broker for the instruments that have a Kite reference.
    #[must_use]
    pub fn new(
        client: KiteClient,
        settings: OrderSettings,
        clock: Arc<dyn Clock>,
        specs: &[InstrumentSpec],
    ) -> Self {
        let mut map = HashMap::new();
        let mut by_symbol = HashMap::new();
        for spec in specs {
            if let Some(r) = kite_ref(spec) {
                by_symbol.insert((r.exchange.clone(), r.tradingsymbol.clone()), spec.id);
                map.insert(spec.id, (r, spec.clone()));
            }
        }
        Self {
            client,
            settings,
            clock,
            specs: map,
            by_symbol,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Tracks the working orders of a restored book (their broker ids come
    /// from the journal).
    pub fn restore(&self, restored: &RestoredState) {
        let mut state = self.lock();
        for r in &restored.intents {
            let working = matches!(
                r.state,
                OrderIntentState::Submitted | OrderIntentState::PartiallyFilled
            );
            if let (true, Some(id)) = (working, &r.broker_order_id) {
                state.tracked.insert(
                    r.intent.id(),
                    Tracked {
                        instrument: r.intent.instrument(),
                        action: r.intent.action(),
                        quantity: r.intent.quantity().value(),
                        reported: r.filled.value(),
                        placement: Placement::parse(id),
                    },
                );
            }
        }
    }

    fn instrument(&self, id: InstrumentId) -> Result<&(KiteRef, InstrumentSpec), BrokerError> {
        self.specs
            .get(&id)
            .ok_or_else(|| BrokerError::Rejected("the instrument has no Kite reference".to_owned()))
    }

    fn track(&self, leg: &Leg, placement: Placement) -> BrokerOrderAck {
        let ack = BrokerOrderAck {
            broker_order_id: BrokerOrderId(placement.broker_id()),
        };
        self.lock().tracked.insert(
            leg.id,
            Tracked {
                instrument: leg.instrument,
                action: leg.action,
                quantity: leg.quantity.value(),
                reported: Decimal::ZERO,
                placement,
            },
        );
        ack
    }

    async fn last_price(&self, kref: &KiteRef) -> Result<Price, BrokerError> {
        let key = format!("{}:{}", kref.exchange, kref.tradingsymbol);
        let data = self
            .client
            .get("/quote/ltp", &[("i", key.clone())])
            .await
            .map_err(|e| match e {
                // No quote means nothing was placed.
                KiteError::Unavailable(d) | KiteError::Unexpected(d) => BrokerError::Rejected(d),
                other => broker_error(other),
            })?;
        data.get(&key)
            .and_then(|q| q.get("last_price"))
            .and_then(decimal)
            .and_then(|p| Price::new(p).ok())
            .ok_or_else(|| BrokerError::Rejected(format!("no last price for {key}")))
    }

    async fn place_gtt(&self, legs: &[Leg]) -> Vec<Result<BrokerOrderAck, BrokerError>> {
        let fail = |e: BrokerError| legs.iter().map(|_| Err(e.clone())).collect();
        if !live_orders_compiled() {
            return fail(not_compiled());
        }
        let Some(first) = legs.first() else {
            return Vec::new();
        };
        let (kref, spec) = match self.instrument(first.instrument) {
            Ok(found) => found.clone(),
            Err(e) => return fail(e),
        };
        let last = match self.last_price(&kref).await {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        let (form, index) = match gtt_form(legs, &kref, &spec, &self.settings, last) {
            Ok(built) => built,
            Err(e) => return fail(BrokerError::Rejected(e)),
        };
        let data = match self.client.post_form("/gtt/triggers", &form, true).await {
            Ok(data) => data,
            Err(e) => return fail(broker_error(e)),
        };
        let Some(trigger) = data.get("trigger_id").map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }) else {
            return fail(BrokerError::Transport(
                "GTT placed without a trigger id".to_owned(),
            ));
        };
        legs.iter()
            .zip(index)
            .map(|(leg, i)| {
                Ok(self.track(
                    leg,
                    Placement::Gtt {
                        trigger: trigger.clone(),
                        leg: i,
                        order: None,
                    },
                ))
            })
            .collect()
    }

    async fn place_order(&self, leg: &Leg) -> Result<BrokerOrderAck, BrokerError> {
        if !live_orders_compiled() {
            return Err(not_compiled());
        }
        let (kref, spec) = self.instrument(leg.instrument)?;
        let form =
            order_form(leg, kref, &spec.venue, &self.settings).map_err(BrokerError::Rejected)?;
        let variety = match self.settings.variety {
            VarietyChoice::Regular => "regular",
            VarietyChoice::Amo => "amo",
            VarietyChoice::Auto if exchange_open(&spec.venue, self.clock.now()) => "regular",
            VarietyChoice::Auto => "amo",
        };
        let data = self
            .client
            .post_form(&format!("/orders/{variety}"), &form, true)
            .await
            .map_err(broker_error)?;
        let id = data
            .get("order_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| BrokerError::Transport("order placed without an order id".to_owned()))?;
        Ok(self.track(leg, Placement::Order(id.to_owned())))
    }

    /// Reads the order book and the GTTs of working orders, and queues fills
    /// and ended orders per instrument for the session.
    pub async fn refresh(&self) -> Result<RefreshReport, KiteError> {
        let book = parse_book(&self.client.get("/orders", &[]).await?);
        let tracked: Vec<(OrderIntentId, Tracked)> = self
            .lock()
            .tracked
            .iter()
            .map(|(id, t)| (*id, t.clone()))
            .collect();
        let mut gtts: BTreeMap<String, Value> = BTreeMap::new();
        let mut report = RefreshReport::default();
        let now = self.clock.now();
        for (intent, mut t) in tracked {
            // A GTT leg: find the order it placed once triggered.
            if let Placement::Gtt {
                trigger,
                leg,
                order,
            } = &mut t.placement
            {
                if order.is_none() {
                    if !gtts.contains_key(trigger.as_str()) {
                        let gtt = self
                            .client
                            .get(&format!("/gtt/triggers/{trigger}"), &[])
                            .await?;
                        gtts.insert(trigger.clone(), gtt);
                    }
                    let gtt = &gtts[trigger.as_str()];
                    let status = gtt.get("status").and_then(Value::as_str).unwrap_or("");
                    let result = gtt.pointer(&format!("/orders/{leg}/result"));
                    let placed = result
                        .and_then(|r| r.pointer("/order_result/order_id"))
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty());
                    match (status, placed) {
                        (_, Some(id)) => *order = Some(id.to_owned()),
                        ("active", None) => continue,
                        // Triggered: the other leg fired and this one lies
                        // dormant; the gateway cancels it when that one fills.
                        ("triggered", None) if result.is_none_or(Value::is_null) => continue,
                        (_, None) => {
                            let reason = result
                                .and_then(|r| r.pointer("/order_result/rejection_reason"))
                                .and_then(Value::as_str)
                                .unwrap_or(status);
                            report.notes.push(format!(
                                "GTT {trigger} leg {leg} ended without an order: {reason}"
                            ));
                            self.end(intent, t.instrument, &mut report);
                            continue;
                        }
                    }
                }
            }
            let order_id = match &t.placement {
                Placement::Order(id)
                | Placement::Gtt {
                    order: Some(id), ..
                } => id.clone(),
                Placement::Gtt { order: None, .. } => continue,
            };
            let Some(o) = book.get(&order_id) else {
                // Not in today's book: an order from an earlier day, or not yet
                // visible. Reconciliation of positions catches a missed fill.
                continue;
            };
            let delta = o.filled - t.reported;
            if delta > Decimal::ZERO {
                let fill = match (Quantity::new(delta), o.average_price.map(Price::new)) {
                    (Ok(quantity), Some(Ok(price))) => Some(BrokerFill {
                        client_order_id: intent,
                        quantity,
                        price,
                        at: o.at.unwrap_or(now),
                    }),
                    _ => None,
                };
                if let Some(fill) = fill {
                    t.reported = o.filled;
                    report.fills += 1;
                    let symbol = self.specs.get(&t.instrument).map_or_else(
                        || t.instrument.to_string(),
                        |(k, _)| format!("{}:{}", k.exchange, k.tradingsymbol),
                    );
                    let role = match t.action {
                        TradeAction::OpenLong | TradeAction::OpenShort => "entry",
                        TradeAction::CloseLong | TradeAction::CloseShort => "exit",
                    };
                    report.fill_notes.push(format!(
                        "{} {} {symbol} @ {} ({role})",
                        transaction_type(t.action),
                        fill.quantity.value().normalize(),
                        fill.price.value().normalize()
                    ));
                    self.lock()
                        .pending
                        .entry(t.instrument)
                        .or_default()
                        .fills
                        .push(fill);
                } else {
                    report
                        .notes
                        .push(format!("order {order_id}: fill without a usable price"));
                }
            }
            let finished = matches!(o.status.as_str(), "COMPLETE" | "CANCELLED" | "REJECTED");
            if finished && t.reported < t.quantity {
                if o.status == "REJECTED" {
                    report
                        .notes
                        .push(format!("order {order_id} rejected by Kite"));
                }
                self.end(intent, t.instrument, &mut report);
            } else if t.reported >= t.quantity {
                self.lock().tracked.remove(&intent);
            } else {
                self.lock().tracked.insert(intent, t);
            }
        }
        Ok(report)
    }

    /// Takes every queued event, per instrument.
    pub fn drain_all(&self) -> Vec<(InstrumentId, BarEvents)> {
        self.lock().pending.drain().collect()
    }

    fn end(&self, intent: OrderIntentId, instrument: InstrumentId, report: &mut RefreshReport) {
        let mut state = self.lock();
        state.tracked.remove(&intent);
        state
            .pending
            .entry(instrument)
            .or_default()
            .expired
            .push(intent);
        report.ended += 1;
    }

    /// Looks up intents whose outcome is unknown in today's order book by
    /// their tag. Orders found are tracked; the others stay unknown.
    pub async fn find_unknown(
        &self,
        intents: &[qd_app::orders::OrderIntent],
    ) -> Result<Vec<(OrderIntentId, ReconciledState)>, KiteError> {
        let book = parse_book(&self.client.get("/orders", &[]).await?);
        let mut found = Vec::new();
        for intent in intents {
            let wanted = tag(intent.id());
            let hit = book
                .iter()
                .find(|(_, o)| o.tag.as_deref() == Some(wanted.as_str()));
            if let Some((order_id, o)) = hit {
                let state = match o.status.as_str() {
                    "REJECTED" => ReconciledState::BrokerRejected,
                    "CANCELLED" => ReconciledState::Cancelled,
                    _ => ReconciledState::Submitted,
                };
                if state == ReconciledState::Submitted {
                    self.lock().tracked.insert(
                        intent.id(),
                        Tracked {
                            instrument: intent.instrument(),
                            action: intent.action(),
                            quantity: intent.quantity().value(),
                            reported: Decimal::ZERO,
                            placement: Placement::Order(order_id.clone()),
                        },
                    );
                }
                found.push((intent.id(), state));
            }
        }
        Ok(found)
    }
}

#[async_trait]
impl BrokerOrderExecutor for KiteBroker {
    async fn submit(&self, request: &BrokerOrderRequest) -> Result<BrokerOrderAck, BrokerError> {
        let leg = Leg::from(request);
        if leg.terms.validity == Validity::GoodTillCancelled {
            return self
                .place_gtt(std::slice::from_ref(&leg))
                .await
                .pop()
                .unwrap_or_else(|| Err(BrokerError::Rejected("no GTT leg".to_owned())));
        }
        self.place_order(&leg).await
    }

    async fn submit_oco(
        &self,
        legs: &[BrokerOrderRequest],
    ) -> Vec<Result<BrokerOrderAck, BrokerError>> {
        let legs: Vec<Leg> = legs.iter().map(Leg::from).collect();
        self.place_gtt(&legs).await
    }

    async fn cancel(&self, client_order_id: OrderIntentId) -> Result<(), BrokerError> {
        let placement = self
            .lock()
            .tracked
            .get(&client_order_id)
            .map(|t| t.placement.clone());
        let Some(placement) = placement else {
            return Err(BrokerError::Rejected(
                "no working Kite order for this intent".to_owned(),
            ));
        };
        let order = match placement {
            Placement::Order(id)
            | Placement::Gtt {
                order: Some(id), ..
            } => id,
            Placement::Gtt { trigger, .. } => {
                let gtt = self
                    .client
                    .get(&format!("/gtt/triggers/{trigger}"), &[])
                    .await
                    .map_err(broker_error)?;
                if gtt.get("status").and_then(Value::as_str) == Some("active") {
                    self.client
                        .delete(&format!("/gtt/triggers/{trigger}"))
                        .await
                        .map_err(broker_error)?;
                }
                // Deleted now, or triggered by the other leg: nothing works any more.
                self.lock().tracked.remove(&client_order_id);
                return Ok(());
            }
        };
        let book = parse_book(
            &self
                .client
                .get("/orders", &[])
                .await
                .map_err(broker_error)?,
        );
        let variety = book.get(&order).map_or("regular", |o| o.variety.as_str());
        self.client
            .delete(&format!("/orders/{variety}/{order}"))
            .await
            .map_err(broker_error)?;
        self.lock().tracked.remove(&client_order_id);
        Ok(())
    }
}

#[async_trait]
impl BrokerAccountReader for KiteBroker {
    /// Holdings (`quantity + t1_quantity`) plus today's delivery trades and
    /// the net of other products, for instruments with a Kite reference.
    async fn positions(&self) -> Result<Vec<BrokerPosition>, BrokerError> {
        let holdings = self
            .client
            .get("/portfolio/holdings", &[])
            .await
            .map_err(broker_error)?;
        let positions = self
            .client
            .get("/portfolio/positions", &[])
            .await
            .map_err(broker_error)?;
        let mut net: BTreeMap<InstrumentId, Decimal> = BTreeMap::new();
        let key = |row: &Value| {
            let text = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
            self.by_symbol
                .get(&(text("exchange"), text("tradingsymbol")))
                .copied()
        };
        let number = |row: &Value, k: &str| row.get(k).and_then(decimal).unwrap_or_default();
        for row in holdings.as_array().into_iter().flatten() {
            if let Some(id) = key(row) {
                *net.entry(id).or_default() += number(row, "quantity") + number(row, "t1_quantity");
            }
        }
        for row in positions
            .get("net")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = key(row) else { continue };
            let delivery = row.get("product").and_then(Value::as_str) == Some("CNC");
            // Delivery positions carried from earlier days are in holdings.
            let quantity = if delivery {
                number(row, "day_buy_quantity") - number(row, "day_sell_quantity")
            } else {
                number(row, "quantity")
            };
            *net.entry(id).or_default() += quantity;
        }
        Ok(net
            .into_iter()
            .filter(|(_, q)| !q.is_zero())
            .map(|(instrument, net_quantity)| BrokerPosition {
                instrument,
                net_quantity,
            })
            .collect())
    }
}

impl SimulatedVenue for KiteBroker {
    fn process_bar(&self, instrument: InstrumentId, _bar: &Bar) -> BarEvents {
        self.lock().pending.remove(&instrument).unwrap_or_default()
    }

    fn process_same_bar_protection(&self, _instrument: InstrumentId, _bar: &Bar) -> BarEvents {
        // Protection works at the broker; nothing is simulated.
        BarEvents::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_ids_round_trip() {
        for p in [
            Placement::Order("2211".to_owned()),
            Placement::Gtt {
                trigger: "77".to_owned(),
                leg: 1,
                order: None,
            },
        ] {
            assert_eq!(Placement::parse(&p.broker_id()), p);
        }
    }

    #[test]
    fn exchange_hours_are_ist_weekdays() {
        use chrono::TimeZone;
        // Friday 2026-09-25: 04:00 UTC is 09:30 IST.
        let at = |d, h, m| Utc.with_ymd_and_hms(2026, 9, d, h, m, 0).unwrap();
        assert!(exchange_open(&Venue::Nse, at(25, 4, 0)));
        assert!(!exchange_open(&Venue::Nse, at(25, 10, 0)));
        assert!(exchange_open(&Venue::Mcx, at(25, 17, 0)));
        assert!(!exchange_open(&Venue::Nse, at(26, 4, 0))); // Saturday
    }
}
