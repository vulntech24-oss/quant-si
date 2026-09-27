//! Test fixtures and fakes for qd-app. Fakes (named `Fake*`) exist only here.

// Test code: a failed unwrap is a failed test. The workspace-wide unwrap/expect
// ban targets runtime code, and clippy's allow-unwrap-in-tests does not reach
// helper functions in integration-test files.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)] // each test binary uses a different subset of these fixtures

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};
use qd_app::decision::{
    DecisionContext, DecisionEngine, EvidencePolicy, JournaledDecision, StrategyVersionInfo,
};
use qd_app::gateway::{GatewayAccount, OrderGateway};
use qd_app::journal::JournalEntry;
use qd_app::live::LivePolicy;
use qd_app::memory::{InMemoryHaltStore, InMemoryJournal};
use qd_app::orders::BrokerOrderRequest;
use qd_app::ports::{
    BrokerError, BrokerOrderAck, BrokerOrderExecutor, BrokerOrderId, Clock, Evidence,
    EvidenceSource, HaltStore, Journal, JournalError, StoreError,
};
use qd_domain::costs::{CostScheduleSet, ScheduleCostModel};
use qd_domain::economics::{OutcomeProbabilities, SlippageAssumption};
use qd_domain::halt::{Halt, HaltState};
use qd_domain::ids::{
    AccountId, DecisionId, InstrumentId, OrderIntentId, SnapshotId, StrategyId, StrategyVersionId,
};
use qd_domain::instrument::{
    AssetClass, CalendarId, Capabilities, CorrelationBucket, InstrumentKind, InstrumentSpec,
    InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::lifecycle::strategy::StrategyStage;
use qd_domain::market::{Bar, BarData, BarSeries};
use qd_domain::num::{Currency, FxRate, Money};
use qd_domain::proposal::{AccountMode, StrategyRef};
use qd_risk::config::{RiskConfig, RiskConfigData};
use qd_risk::gate::AccountRiskState;
use qd_strategy::regime::RegimeClassifier;
use qd_strategy::strategy::run_strategy;
use qd_strategy::trend_pullback::TrendPullback;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub const COSTS: &str = include_str!("../../../../config/costs/india-zerodha.toml");
pub const RISK: &str = include_str!("../../../../config/risk.toml");

pub fn start() -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 6, 1).unwrap()
}

pub fn day(n: usize) -> NaiveDate {
    start() + Days::new(n as u64)
}

pub fn at_close(date: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&date.and_hms_opt(10, 0, 0).unwrap())
}

/// A clock tests can move.
pub struct FakeClock(pub Mutex<DateTime<Utc>>);

impl FakeClock {
    pub fn at(at: DateTime<Utc>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(at)))
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}

/// Records every request; can be told to fail.
#[derive(Default)]
pub struct FakeExecutor {
    pub submitted: Mutex<Vec<BrokerOrderRequest>>,
    pub cancelled: Mutex<Vec<OrderIntentId>>,
    pub reject: AtomicBool,
    pub transport_error: AtomicBool,
}

impl FakeExecutor {
    pub fn submits(&self) -> usize {
        self.submitted.lock().unwrap().len()
    }
}

#[async_trait]
impl BrokerOrderExecutor for FakeExecutor {
    async fn submit(&self, request: &BrokerOrderRequest) -> Result<BrokerOrderAck, BrokerError> {
        if self.reject.load(Ordering::SeqCst) {
            return Err(BrokerError::Rejected("margin".to_owned()));
        }
        self.submitted.lock().unwrap().push(request.clone());
        if self.transport_error.load(Ordering::SeqCst) {
            return Err(BrokerError::Transport("timeout".to_owned()));
        }
        Ok(BrokerOrderAck {
            broker_order_id: BrokerOrderId(request.client_order_id().to_string()),
        })
    }

    async fn cancel(&self, id: OrderIntentId) -> Result<(), BrokerError> {
        self.cancelled.lock().unwrap().push(id);
        Ok(())
    }
}

/// A journal that can be switched to fail every write.
#[derive(Default)]
pub struct FakeJournal {
    pub inner: InMemoryJournal,
    pub fail: AtomicBool,
    pub writes: AtomicUsize,
}

#[async_trait]
impl Journal for FakeJournal {
    async fn append(&self, entry: &JournalEntry) -> Result<u64, JournalError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(JournalError("disk full".to_owned()));
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.append(entry).await
    }
}

/// A halt store that cannot be read.
pub struct FakeBrokenHaltStore;

#[async_trait]
impl HaltStore for FakeBrokenHaltStore {
    async fn load(&self) -> Result<Vec<Halt>, StoreError> {
        Err(StoreError("database unreachable".to_owned()))
    }
    async fn record(&self, _halt: &Halt) -> Result<(), StoreError> {
        Err(StoreError("database unreachable".to_owned()))
    }
}

/// A fixed evidence table.
pub struct FakeEvidence(pub Option<u32>);

impl EvidenceSource for FakeEvidence {
    fn evidence(&self, _version: StrategyVersionId, _setup_type: &str) -> Option<Evidence> {
        let n = self.0?;
        Some(Evidence {
            probabilities: OutcomeProbabilities::new(
                dec!(0.45),
                dec!(0.40),
                dec!(0.15),
                "fixture",
                n,
            )
            .unwrap(),
            time_exit_r: dec!(0.2),
        })
    }
}

pub fn spec() -> InstrumentSpec {
    static ID: OnceLock<InstrumentId> = OnceLock::new();
    let id = *ID.get_or_init(|| InstrumentId::new_at(at_close(start())));
    InstrumentSpec::new(InstrumentSpecData {
        id,
        version: 1,
        effective_from: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
        effective_to: None,
        symbol: "TEST-EQ".to_owned(),
        venue: Venue::Nse,
        asset_class: AssetClass::Equity,
        kind: InstrumentKind::CashEquity,
        underlying: None,
        currency: Currency::INR,
        tick_size: dec!(0.05),
        lot_size: Decimal::ONE,
        multiplier: Decimal::ONE,
        quantity_step: Decimal::ONE,
        min_quantity: Decimal::ONE,
        expiry: None,
        calendar_id: CalendarId("nse".to_owned()),
        correlation_bucket: CorrelationBucket("india_equity".to_owned()),
        broker_refs: vec![],
        capabilities: Capabilities {
            can_short_overnight: false,
            supports_market_orders: true,
            requires_market_protection: true,
            protection_modes: vec![ProtectionMode::BrokerOco],
            products: vec![ProductType::Delivery],
            order_types: vec![
                OrderType::Limit,
                OrderType::StopLimit,
                OrderType::StopMarket,
                OrderType::Market,
            ],
            validities: vec![Validity::Day, Validity::GoodTillCancelled],
        },
    })
    .unwrap()
}

/// An uptrend whose last bar pulls back to the 20-day average: a trend-pullback setup.
pub fn setup_series() -> BarSeries {
    let wiggle = [0, 6, 10, 6, 0, -6, -10, -6, 0, 3];
    let mut bars: Vec<Bar> = (0..259)
        .map(|n| {
            let close = dec!(100) + dec!(0.25) * Decimal::from(n) + Decimal::new(wiggle[n % 10], 1);
            Bar::new(BarData {
                date: day(n),
                open: close,
                high: close + dec!(0.5),
                low: close - dec!(0.5),
                close,
                volume: dec!(10000),
            })
            .unwrap()
        })
        .collect();
    let close = bars.last().unwrap().close().value() - dec!(2);
    bars.push(
        Bar::new(BarData {
            date: day(259),
            open: close + dec!(0.5),
            high: close + dec!(0.8),
            low: close - dec!(1.5),
            close,
            volume: dec!(10000),
        })
        .unwrap(),
    );
    BarSeries::new(spec().id, day(259), bars).unwrap()
}

pub fn decision_date() -> NaiveDate {
    day(259)
}

pub fn risk() -> RiskConfig {
    RiskConfig::new(toml::from_str::<RiskConfigData>(RISK).unwrap()).unwrap()
}

pub fn costs() -> ScheduleCostModel {
    ScheduleCostModel::new(toml::from_str::<CostScheduleSet>(COSTS).unwrap()).unwrap()
}

pub fn account_id() -> AccountId {
    static ID: OnceLock<AccountId> = OnceLock::new();
    *ID.get_or_init(|| AccountId::new_at(at_close(start())))
}

pub fn strategy_info(stage: StrategyStage) -> StrategyVersionInfo {
    static IDS: OnceLock<(StrategyId, StrategyVersionId)> = OnceLock::new();
    let (strategy_id, version_id) = *IDS.get_or_init(|| {
        (
            StrategyId::new_at(at_close(start())),
            StrategyVersionId::new_at(at_close(start())),
        )
    });
    let strategy = TrendPullback::v1();
    StrategyVersionInfo {
        reference: StrategyRef {
            strategy_id,
            name: "Trend pullback".to_owned(),
            version_id,
            version_number: 1,
            logic_version: TrendPullback::LOGIC_VERSION.to_owned(),
            git_sha: "test".to_owned(),
        },
        stage,
        rr_floor: strategy.params().rr_floor,
        slippage: SlippageAssumption::new("slip-v1", dec!(0.10)).unwrap(),
    }
}

pub fn account_state(mode: AccountMode, halts: HaltState) -> AccountRiskState {
    let inr = |a: Decimal| Money::new(a, Currency::INR);
    AccountRiskState {
        account: account_id(),
        mode,
        equity: inr(dec!(1000000)),
        equity_at_day_start: inr(dec!(1000000)),
        equity_at_week_start: inr(dec!(1000000)),
        high_water_mark: inr(dec!(1000000)),
        consecutive_losses: 0,
        open_risk: vec![],
        halts,
    }
}

/// Runs the real pipeline and returns the journaled decision.
pub async fn decide(
    mode: AccountMode,
    stage: StrategyStage,
    evidence: &dyn EvidenceSource,
    policy: EvidencePolicy,
    journal: &dyn Journal,
) -> Result<Option<JournaledDecision>, JournalError> {
    let (risk, costs, spec, series) = (risk(), costs(), spec(), setup_series());
    let evaluation =
        run_strategy(&TrendPullback::v1(), &spec, &series, &RegimeClassifier::V1).unwrap();
    let info = strategy_info(stage);
    let state = account_state(mode, HaltState::Known(vec![]));
    let at = at_close(decision_date());
    let engine = DecisionEngine::new(&risk, &costs, evidence, policy);
    engine
        .decide_and_journal(
            &DecisionContext {
                decision_id: DecisionId::new_at(at),
                at,
                expected_last_completed: decision_date(),
                spec: &spec,
                product: ProductType::Delivery,
                fx: FxRate::identity(Currency::INR, at),
                strategy: &info,
                evaluation: &evaluation,
                series: &series,
                account: &state,
                already_in_position: false,
                snapshot_id: SnapshotId::new_at(at),
                calendar_version: "test",
            },
            journal,
        )
        .await
}

pub struct Harness {
    pub executor: Arc<FakeExecutor>,
    pub journal: Arc<FakeJournal>,
    pub halts: Arc<InMemoryHaltStore>,
    pub clock: Arc<FakeClock>,
    pub gateway: Arc<OrderGateway>,
}

pub fn harness(mode: AccountMode, live: LivePolicy, live_armed: bool) -> Harness {
    let executor = Arc::new(FakeExecutor::default());
    let journal = Arc::new(FakeJournal::default());
    let halts = Arc::new(InMemoryHaltStore::new());
    let clock = FakeClock::at(at_close(decision_date()));
    let gateway = Arc::new(OrderGateway::new(
        GatewayAccount {
            id: account_id(),
            mode,
            live_armed,
        },
        executor.clone(),
        journal.clone(),
        halts.clone(),
        clock.clone(),
        live,
        vec![spec()],
    ));
    Harness {
        executor,
        journal,
        halts,
        clock,
        gateway,
    }
}
