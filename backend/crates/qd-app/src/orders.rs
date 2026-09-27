//! Order intents, entry authorizations and broker order requests (INV-01, INV-03).
//!
//! Two types make the one-order-path rule structural:
//!
//! - [`EntryAuthorization`] can only be created inside this crate, by the
//!   Decision Engine after the Risk Gate approved an entry. Every
//!   risk-increasing [`OrderIntent`] needs one, so no entry can bypass the
//!   Risk Gate (INV-03).
//! - [`BrokerOrderRequest`] can only be created inside this crate, by the
//!   Order Gateway. Broker adapters can read it but nobody outside the gateway
//!   can build one, so nothing else can place an order (INV-01).

use chrono::{DateTime, Utc};
use qd_domain::action::{EntryAction, ExitAction, RiskEffect, TradeAction};
use qd_domain::ids::{
    AccountId, DecisionId, InstrumentId, OrderIntentId, PositionId, StrategyVersionId,
};
use qd_domain::instrument::{OrderType, ProductType, Validity};
use qd_domain::num::{Price, Quantity};
use qd_domain::outcome::ExitReason;
use serde::{Deserialize, Serialize};

/// Permission to open a position, issued only after a Risk Gate approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct EntryAuthorization {
    decision: DecisionId,
    account: AccountId,
    instrument: InstrumentId,
    strategy_version: Option<StrategyVersionId>,
    action: EntryAction,
    quantity: Quantity,
}

impl EntryAuthorization {
    pub(crate) const fn new(
        decision: DecisionId,
        account: AccountId,
        instrument: InstrumentId,
        strategy_version: Option<StrategyVersionId>,
        action: EntryAction,
        quantity: Quantity,
    ) -> Self {
        Self {
            decision,
            account,
            instrument,
            strategy_version,
            action,
            quantity,
        }
    }

    /// The approving decision.
    #[must_use]
    pub const fn decision(&self) -> DecisionId {
        self.decision
    }

    /// Account.
    #[must_use]
    pub const fn account(&self) -> AccountId {
        self.account
    }

    /// Instrument.
    #[must_use]
    pub const fn instrument(&self) -> InstrumentId {
        self.instrument
    }

    /// Strategy version, if any.
    #[must_use]
    pub const fn strategy_version(&self) -> Option<StrategyVersionId> {
        self.strategy_version
    }

    /// Approved action.
    #[must_use]
    pub const fn action(&self) -> EntryAction {
        self.action
    }

    /// Approved quantity: the maximum the entry may use.
    #[must_use]
    pub const fn quantity(&self) -> Quantity {
        self.quantity
    }
}

/// Why an order exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "purpose", rename_all = "snake_case")]
pub enum OrderPurpose {
    /// Opens a position under an entry authorization.
    Entry {
        /// The authorizing decision.
        decision: DecisionId,
    },
    /// Protective stop for a position.
    ProtectiveStop,
    /// Target for a position.
    Target,
    /// Closes (part of) a position.
    Exit {
        /// Why.
        reason: ExitReason,
    },
}

/// Price terms of an order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderTerms {
    /// Order type.
    pub order_type: OrderType,
    /// Limit price, for limit and stop-limit orders.
    pub limit: Option<Price>,
    /// Trigger price, for stop orders.
    pub trigger: Option<Price>,
    /// Validity.
    pub validity: Validity,
    /// Product.
    pub product: ProductType,
}

/// An order the Gateway is asked to place. Journaled before any broker call (INV-05).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OrderIntent {
    id: OrderIntentId,
    account: AccountId,
    instrument: InstrumentId,
    strategy_version: Option<StrategyVersionId>,
    position: Option<PositionId>,
    action: TradeAction,
    quantity: Quantity,
    terms: OrderTerms,
    purpose: OrderPurpose,
    oco_group: Option<PositionId>,
    created_at: DateTime<Utc>,
    authorized_quantity: Option<Quantity>,
}

impl OrderIntent {
    /// An entry under an authorization. The quantity is the authorized one.
    #[must_use]
    pub fn entry(
        id: OrderIntentId,
        authorization: &EntryAuthorization,
        position: PositionId,
        terms: OrderTerms,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            account: authorization.account,
            instrument: authorization.instrument,
            strategy_version: authorization.strategy_version,
            position: Some(position),
            action: authorization.action.action(),
            quantity: authorization.quantity,
            terms,
            purpose: OrderPurpose::Entry {
                decision: authorization.decision,
            },
            oco_group: None,
            created_at,
            authorized_quantity: Some(authorization.quantity),
        }
    }

    /// A risk-reducing order for a position: exit, protective stop or target.
    /// Orders sharing `oco_group` are one-cancels-other.
    #[allow(clippy::too_many_arguments)] // each field is a distinct, required order attribute
    #[must_use]
    pub fn reducing(
        id: OrderIntentId,
        account: AccountId,
        instrument: InstrumentId,
        strategy_version: Option<StrategyVersionId>,
        position: PositionId,
        action: ExitAction,
        quantity: Quantity,
        terms: OrderTerms,
        purpose: OrderPurpose,
        oco_group: Option<PositionId>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id,
            account,
            instrument,
            strategy_version,
            position: Some(position),
            action: action.action(),
            quantity,
            terms,
            purpose,
            oco_group,
            created_at,
            authorized_quantity: None,
        }
    }

    /// Intent id; also the client order id sent to the broker (idempotency key).
    #[must_use]
    pub const fn id(&self) -> OrderIntentId {
        self.id
    }

    /// Account.
    #[must_use]
    pub const fn account(&self) -> AccountId {
        self.account
    }

    /// Instrument.
    #[must_use]
    pub const fn instrument(&self) -> InstrumentId {
        self.instrument
    }

    /// Strategy version.
    #[must_use]
    pub const fn strategy_version(&self) -> Option<StrategyVersionId> {
        self.strategy_version
    }

    /// Position.
    #[must_use]
    pub const fn position(&self) -> Option<PositionId> {
        self.position
    }

    /// Action.
    #[must_use]
    pub const fn action(&self) -> TradeAction {
        self.action
    }

    /// Quantity.
    #[must_use]
    pub const fn quantity(&self) -> Quantity {
        self.quantity
    }

    /// Price terms.
    #[must_use]
    pub const fn terms(&self) -> OrderTerms {
        self.terms
    }

    /// Purpose.
    #[must_use]
    pub const fn purpose(&self) -> OrderPurpose {
        self.purpose
    }

    /// OCO group.
    #[must_use]
    pub const fn oco_group(&self) -> Option<PositionId> {
        self.oco_group
    }

    /// Creation time.
    #[must_use]
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Risk effect of the action.
    #[must_use]
    pub const fn risk_effect(&self) -> RiskEffect {
        self.action.risk_effect()
    }

    pub(crate) const fn authorized_quantity(&self) -> Option<Quantity> {
        self.authorized_quantity
    }

    /// Reads an intent back from its journal JSON. Crate-private: only the
    /// restore path may rebuild intents, and a restored intent is never sent
    /// again (it was sent before it was journaled as accepted).
    pub(crate) fn from_journal(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        let d: IntentData = serde_json::from_value(value.clone())?;
        Ok(Self {
            id: d.id,
            account: d.account,
            instrument: d.instrument,
            strategy_version: d.strategy_version,
            position: d.position,
            action: d.action,
            quantity: d.quantity,
            terms: d.terms,
            purpose: d.purpose,
            oco_group: d.oco_group,
            created_at: d.created_at,
            authorized_quantity: d.authorized_quantity,
        })
    }
}

/// The journal shape of an [`OrderIntent`]. Private so that deserializing
/// cannot become a public way to build an authorized entry.
#[derive(Deserialize)]
struct IntentData {
    id: OrderIntentId,
    account: AccountId,
    instrument: InstrumentId,
    strategy_version: Option<StrategyVersionId>,
    position: Option<PositionId>,
    action: TradeAction,
    quantity: Quantity,
    terms: OrderTerms,
    purpose: OrderPurpose,
    oco_group: Option<PositionId>,
    created_at: DateTime<Utc>,
    authorized_quantity: Option<Quantity>,
}

/// An order as sent to a broker. Only the Order Gateway can build one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BrokerOrderRequest {
    client_order_id: OrderIntentId,
    account: AccountId,
    instrument: InstrumentId,
    symbol: String,
    action: TradeAction,
    quantity: Quantity,
    terms: OrderTerms,
    oco_group: Option<PositionId>,
}

impl BrokerOrderRequest {
    pub(crate) fn from_intent(intent: &OrderIntent, symbol: &str) -> Self {
        Self {
            client_order_id: intent.id,
            account: intent.account,
            instrument: intent.instrument,
            symbol: symbol.to_owned(),
            action: intent.action,
            quantity: intent.quantity,
            terms: intent.terms,
            oco_group: intent.oco_group,
        }
    }

    /// Client order id (the intent id); brokers use it to deduplicate.
    #[must_use]
    pub const fn client_order_id(&self) -> OrderIntentId {
        self.client_order_id
    }

    /// Account.
    #[must_use]
    pub const fn account(&self) -> AccountId {
        self.account
    }

    /// Instrument.
    #[must_use]
    pub const fn instrument(&self) -> InstrumentId {
        self.instrument
    }

    /// Exchange symbol.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Action. Only broker adapters map it to a broker transaction type.
    #[must_use]
    pub const fn action(&self) -> TradeAction {
        self.action
    }

    /// Quantity.
    #[must_use]
    pub const fn quantity(&self) -> Quantity {
        self.quantity
    }

    /// Price terms.
    #[must_use]
    pub const fn terms(&self) -> OrderTerms {
        self.terms
    }

    /// OCO group: at most one order of a group may fill.
    #[must_use]
    pub const fn oco_group(&self) -> Option<PositionId> {
        self.oco_group
    }
}
