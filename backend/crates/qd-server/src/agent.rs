//! The AI agent in the server (ADR 0016).
//!
//! - [`KiteMarket`]: the agent's market access: Zerodha's instrument list,
//!   real-time quotes (`/quote/ohlc`) and daily-bar imports with the usual
//!   data-quality checks. Read-only at the broker.
//! - [`LiveDesk`]: the live book as an `AgentDesk`, used only when the
//!   server file's `[agent_live]` allows it and `[live]` is configured.
//!   The paper runner is the default desk.
//! - [`DynAgent`]: the `AgentControl` port: runs (one at a time), traces,
//!   predictions, the scorecard and scoring. Reads the effective settings
//!   on every call.
//! - [`spawn_schedule`]: the daily research run and the market-hours
//!   monitoring runs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use qd_agent::agent::{RunKind, evaluate_due};
use qd_agent::llm::{ChatModel, ProviderModel};
use qd_agent::tools::{MarketAccess, Tools};
use qd_app::ports::{
    AgentControl, AgentDesk, AgentExecution, Clock, JournalReader, PortfolioReader, RunLock,
    StoreError,
};
use qd_app::session::AgentEntry;
use qd_broker_kite::market::{KiteInstrument, instruments, kite_ref};
use qd_domain::ids::{AgentRunId, InstrumentId, PositionId, StrategyVersionId};
use qd_domain::instrument::{
    AssetClass, BrokerRef, CalendarId, Capabilities, CorrelationBucket, InstrumentKind,
    InstrumentSpec, InstrumentSpecData, OrderType, ProductType, ProtectionMode, Validity, Venue,
};
use qd_domain::lifecycle::strategy::StrategyStage;
use rust_decimal::Decimal;
use serde_json::{Value, json};

use crate::kite::{import_spec_bars, latest_specs};
use crate::runtime::Runtime;

/// Calendar days of history imported for a new instrument (the feature set
/// recorded with every decision needs 220 completed bars).
const NEW_INSTRUMENT_HISTORY_DAYS: i64 = 500;

fn error(e: impl std::fmt::Display) -> StoreError {
    StoreError(e.to_string())
}

/// Instrument lists by exchange, with the day they were downloaded.
type ListCache = HashMap<String, (NaiveDate, Arc<Vec<KiteInstrument>>)>;

/// The agent's market access at Zerodha.
pub struct KiteMarket {
    runtime: Arc<Runtime>,
    lists: Mutex<ListCache>,
}

impl std::fmt::Debug for KiteMarket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KiteMarket").finish_non_exhaustive()
    }
}

impl KiteMarket {
    /// Market access over the runtime.
    #[must_use]
    pub fn new(runtime: Arc<Runtime>) -> Self {
        Self {
            runtime,
            lists: Mutex::new(HashMap::new()),
        }
    }

    async fn client(&self) -> Result<qd_broker_kite::client::KiteClient, String> {
        let client = self.runtime.kite_client().await.map_err(|e| e.0)?;
        if client.logged_in() {
            Ok(client)
        } else {
            Err("not logged in with Zerodha today (Broker → Login with Zerodha)".to_owned())
        }
    }

    /// One exchange's instrument list, downloaded once a day.
    async fn list(&self, exchange: &str) -> Result<Arc<Vec<KiteInstrument>>, String> {
        let today = self.runtime.clock.now().date_naive();
        if let Ok(lists) = self.lists.lock() {
            if let Some((day, rows)) = lists.get(exchange) {
                if *day == today {
                    return Ok(rows.clone());
                }
            }
        }
        let client = self.client().await?;
        let rows = Arc::new(
            instruments(&client, exchange)
                .await
                .map_err(|e| e.to_string())?,
        );
        if let Ok(mut lists) = self.lists.lock() {
            lists.insert(exchange.to_owned(), (today, rows.clone()));
        }
        Ok(rows)
    }

    async fn find(&self, exchange: &str, tradingsymbol: &str) -> Result<KiteInstrument, String> {
        self.list(exchange)
            .await?
            .iter()
            .find(|r| {
                r.instrument_type == "EQ"
                    && r.segment == exchange
                    && r.tradingsymbol.eq_ignore_ascii_case(tradingsymbol)
            })
            .cloned()
            .ok_or_else(|| format!("{exchange}:{tradingsymbol} is not a listed equity at Zerodha"))
    }

    async fn specs(&self) -> Result<Vec<InstrumentSpec>, String> {
        latest_specs(&self.runtime, self.runtime.clock.now().date_naive())
            .await
            .map_err(|e| e.0)
    }

    async fn import(&self, spec: &InstrumentSpec, history_days: i64) -> Result<Value, String> {
        let kref = kite_ref(spec).ok_or("the instrument has no Zerodha reference")?;
        let token = match kref.token {
            Some(t) => t,
            None => {
                self.find(&kref.exchange, &kref.tradingsymbol)
                    .await?
                    .instrument_token
            }
        };
        let client = self.client().await?;
        let e = self.runtime.effective().await.map_err(|e| e.0)?;
        import_spec_bars(&self.runtime, &e, &client, spec, &token, history_days)
            .await
            .map(compact_issues)
            .map_err(|e| e.to_string())
    }
}

/// An import report with its data issues counted by kind (plus the first
/// few), so a long history does not flood the model's context.
fn compact_issues(mut report: Value) -> Value {
    let Some(issues) = report.get("issues").and_then(Value::as_array).cloned() else {
        return report;
    };
    let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for i in &issues {
        let kind = i
            .get("issue")
            .and_then(Value::as_str)
            .unwrap_or("other")
            .to_owned();
        *counts.entry(kind).or_default() += 1;
    }
    let first: Vec<Value> = issues
        .iter()
        .filter(|i| i.get("issue").and_then(Value::as_str) != Some("bar_on_holiday"))
        .take(5)
        .cloned()
        .collect();
    if let Some(obj) = report.as_object_mut() {
        obj.insert(
            "issues".to_owned(),
            json!({"counts": counts, "first": first}),
        );
    }
    report
}

/// The spec QuantDesk records for an NSE or BSE cash equity from Zerodha's
/// list: delivery only, no overnight shorts, the exchange's tick size.
pub fn equity_spec(
    row: &KiteInstrument,
    venue: Venue,
    now: DateTime<Utc>,
) -> Result<InstrumentSpec, String> {
    let tick: Decimal = row
        .tick_size
        .trim()
        .parse()
        .map_err(|_| format!("bad tick size {}", row.tick_size))?;
    let calendar = match venue {
        Venue::Bse => "bse",
        _ => "nse",
    };
    InstrumentSpec::new(InstrumentSpecData {
        id: InstrumentId::new_at(now),
        version: 1,
        effective_from: NaiveDate::from_ymd_opt(2000, 1, 1).ok_or("bad date")?,
        effective_to: None,
        symbol: row.tradingsymbol.clone(),
        venue,
        asset_class: AssetClass::Equity,
        kind: InstrumentKind::CashEquity,
        underlying: None,
        currency: qd_domain::num::Currency::INR,
        tick_size: tick,
        lot_size: Decimal::ONE,
        multiplier: Decimal::ONE,
        quantity_step: Decimal::ONE,
        min_quantity: Decimal::ONE,
        expiry: None,
        calendar_id: CalendarId(calendar.to_owned()),
        correlation_bucket: CorrelationBucket("india_equity".to_owned()),
        broker_refs: vec![BrokerRef {
            broker: "kite".to_owned(),
            symbol: row.tradingsymbol.clone(),
            token: Some(row.instrument_token.clone()),
        }],
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
    .map_err(|e| e.to_string())
}

#[async_trait]
impl MarketAccess for KiteMarket {
    async fn search(&self, query: &str) -> Result<Value, String> {
        let q = query.trim().to_uppercase();
        if q.len() < 2 {
            return Err("query too short".to_owned());
        }
        let tracked: Vec<String> = self
            .specs()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.symbol.clone())
            .collect();
        let mut hits = Vec::new();
        for exchange in ["NSE", "BSE"] {
            let rows = self.list(exchange).await?;
            hits.extend(
                rows.iter()
                    .filter(|r| r.instrument_type == "EQ" && r.segment == exchange)
                    .filter(|r| {
                        r.tradingsymbol.to_uppercase().contains(&q)
                            || r.name.to_uppercase().contains(&q)
                    })
                    .take(10)
                    .map(|r| {
                        json!({
                            "exchange": exchange,
                            "tradingsymbol": r.tradingsymbol,
                            "name": r.name,
                            "tracked": tracked.contains(&r.tradingsymbol),
                        })
                    }),
            );
        }
        Ok(Value::Array(hits))
    }

    async fn add_instrument(&self, exchange: &str, tradingsymbol: &str) -> Result<Value, String> {
        let venue = match exchange {
            "NSE" => Venue::Nse,
            "BSE" => Venue::Bse,
            _ => return Err("exchange must be NSE or BSE".to_owned()),
        };
        if let Some(existing) = self.specs().await?.into_iter().find(|s| {
            s.venue == venue
                && kite_ref(s).is_some_and(|k| k.tradingsymbol.eq_ignore_ascii_case(tradingsymbol))
        }) {
            let import = self.import(&existing, NEW_INSTRUMENT_HISTORY_DAYS).await;
            return Ok(
                json!({"symbol": existing.symbol, "already_tracked": true, "import": import.unwrap_or_else(|e| json!({"error": e}))}),
            );
        }
        let row = self.find(exchange, tradingsymbol).await?;
        let now = self.runtime.clock.now();
        let spec = equity_spec(&row, venue, now)?;
        if self
            .specs()
            .await?
            .iter()
            .any(|s| s.symbol.eq_ignore_ascii_case(&spec.symbol))
        {
            return Err(format!(
                "{} is already tracked on another venue; use that one",
                spec.symbol
            ));
        }
        self.runtime
            .stores
            .market
            .add_instrument(&spec)
            .await
            .map_err(|e| e.to_string())?;
        qd_app::ports::AuditLog::record(
            self.runtime.stores.audit.as_ref(),
            "ai-agent",
            "instrument.add",
            json!({ "id": spec.id, "version": spec.version, "symbol": spec.symbol, "source": "kite" }),
        )
        .await
        .map_err(|e| e.to_string())?;
        let import = self.import(&spec, NEW_INSTRUMENT_HISTORY_DAYS).await;
        Ok(json!({
            "symbol": spec.symbol,
            "exchange": exchange,
            "name": row.name,
            "import": import.unwrap_or_else(|e| json!({"error": e})),
        }))
    }

    async fn quotes(&self, keys: &[String]) -> Result<Value, String> {
        let client = self.client().await?;
        let query: Vec<(&str, String)> = keys.iter().take(20).map(|k| ("i", k.clone())).collect();
        // The client returns the envelope's `data`.
        let data = client
            .get("/quote/ohlc", &query)
            .await
            .map_err(|e| e.to_string())?;
        let missing: Vec<&String> = keys
            .iter()
            .filter(|k| data.get(k.as_str()).is_none())
            .collect();
        Ok(json!({
            "as_of": self.runtime.clock.now(),
            "quotes": data,
            "missing": missing,
            "note": "ohlc.close is the previous session's close",
        }))
    }

    async fn refresh_bars(&self, instrument: InstrumentId) -> Result<Value, String> {
        let spec = self
            .specs()
            .await?
            .into_iter()
            .find(|s| s.id == instrument)
            .ok_or("unknown instrument")?;
        let e = self.runtime.effective().await.map_err(|e| e.0)?;
        self.import(&spec, e.kite.history_days.max(NEW_INSTRUMENT_HISTORY_DAYS))
            .await
    }
}

/// The live book as the agent's desk (server file `[agent_live]` only).
pub struct LiveDesk(pub Arc<Runtime>);

impl LiveDesk {
    async fn runner(&self) -> Result<qd_broker_kite::runner::LiveRunner, StoreError> {
        self.0
            .live_runner()
            .await?
            .ok_or_else(|| StoreError("live trading is not configured ([live])".to_owned()))
    }
}

#[async_trait]
impl AgentDesk for LiveDesk {
    fn book(&self) -> &'static str {
        "live"
    }

    async fn account(&self) -> Result<Value, StoreError> {
        let runner = self.runner().await?;
        let state = runner.state().await.map_err(error)?;
        let live = self
            .0
            .config
            .file
            .live
            .as_ref()
            .ok_or_else(|| StoreError("live trading is not configured".to_owned()))?;
        let mut book = qd_app::runs::book_json(live.account_id, &state);
        let e = self.0.effective().await?;
        if let Some(obj) = book.as_object_mut() {
            obj.insert(
                "initial_equity".to_owned(),
                Value::String(live.initial_equity.to_string()),
            );
            obj.insert(
                "risk_limits".to_owned(),
                serde_json::to_value(&*e.risk).map_err(error)?,
            );
        }
        Ok(book)
    }

    async fn enter(&self, entry: AgentEntry) -> Result<AgentExecution, StoreError> {
        let runner = self.runner().await?;
        let e = self.0.effective().await?;
        let client = self.0.kite_client().await?;
        runner
            .agent_enter(client, e.kite.orders(), &entry)
            .await
            .map_err(error)
    }

    async fn exit(
        &self,
        position: PositionId,
        agent: StrategyVersionId,
    ) -> Result<Value, StoreError> {
        let runner = self.runner().await?;
        let e = self.0.effective().await?;
        let client = self.0.kite_client().await?;
        let update = runner
            .agent_exit(client, e.kite.orders(), position, agent)
            .await
            .map_err(error)?;
        serde_json::to_value(update).map_err(error)
    }
}

/// The agent service over the runtime.
pub struct DynAgent {
    /// Runtime.
    pub runtime: Arc<Runtime>,
    /// Market access.
    pub market: Arc<KiteMarket>,
    /// Portfolio views.
    pub portfolio: Option<Arc<dyn PortfolioReader>>,
}

impl std::fmt::Debug for DynAgent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynAgent").finish_non_exhaustive()
    }
}

impl DynAgent {
    fn live_book(&self) -> bool {
        self.runtime.config.file.agent_live.enabled && self.runtime.config.file.live.is_some()
    }

    fn model_name(e: &crate::runtime::Effective) -> String {
        format!("{}:{}", e.agent.provider, e.agent.model.trim())
    }

    async fn tools(&self, run: AgentRunId) -> Result<Tools, StoreError> {
        let rt = &self.runtime;
        let e = rt.effective().await?;
        if !e.agent.enabled {
            return Err(StoreError(
                "the AI agent is off (Settings → AI agent)".to_owned(),
            ));
        }
        let provider = e
            .agent
            .provider_kind()
            .ok_or_else(|| StoreError("unknown agent provider".to_owned()))?;
        let key = rt.secret(e.agent.key_name()).await.ok_or_else(|| {
            StoreError(format!(
                "the {} API key is not set (Settings → API keys → {})",
                e.agent.provider,
                e.agent.key_name()
            ))
        })?;
        let model: Arc<dyn ChatModel> = Arc::new(
            ProviderModel::new(
                provider,
                &e.agent.model,
                rt.agent_base.as_deref().unwrap_or(provider.default_base()),
                key,
                std::time::Duration::from_secs(e.agent.call_timeout_seconds),
            )
            .map_err(error)?,
        );
        let live = self.live_book();
        let desk: Arc<dyn AgentDesk> = if live {
            Arc::new(LiveDesk(rt.clone()))
        } else {
            Arc::new(rt.paper_runner().await?.ok_or_else(|| {
                StoreError(
                    "paper trading is off (Settings → Paper trading); the agent trades the paper book"
                        .to_owned(),
                )
            })?)
        };
        let (card, _, _) = Tools::record_of(rt.stores.journal.as_ref(), Some(model.name()))
            .await
            .map_err(StoreError)?;
        let (stage, _) = rt.config.file.agent_live.stage(live, &card);
        Ok(Tools {
            model,
            market: self.market.clone(),
            data: rt.stores.market.clone(),
            desk,
            portfolio: self.portfolio.clone(),
            journal: rt.stores.journal.clone(),
            reader: rt.stores.journal.clone(),
            clock: rt.clock.clone(),
            settings: e.agent.limits.clone(),
            stage,
            run,
        })
    }

    async fn evaluate_now(&self) -> Result<Value, StoreError> {
        let s = &self.runtime.stores;
        evaluate_due(
            s.journal.as_ref(),
            s.market.as_ref(),
            s.journal.as_ref(),
            self.runtime.clock.as_ref(),
        )
        .await
        .map_err(StoreError)
    }
}

#[async_trait]
impl AgentControl for DynAgent {
    async fn status(&self) -> Result<Value, StoreError> {
        let rt = &self.runtime;
        let e = rt.effective().await?;
        let model = Self::model_name(&e);
        let (card, _, _) = Tools::record_of(rt.stores.journal.as_ref(), Some(&model))
            .await
            .map_err(StoreError)?;
        let live = self.live_book();
        let gate = &rt.config.file.agent_live;
        let (stage, missing) = gate.stage(true, &card);
        let current = if live { stage } else { StrategyStage::Paper };
        Ok(json!({
            "enabled": e.agent.enabled,
            "provider": e.agent.provider,
            "model": model,
            "key_set": rt.secret(e.agent.key_name()).await.is_some(),
            "book": if live { "live" } else { "paper" },
            "stage": current,
            "strategy_version": qd_agent::config::agent_ref(&model).version_id,
            "live_gate": {
                "enabled_in_server_file": gate.enabled,
                "live_book_configured": rt.config.file.live.is_some(),
                "thresholds": gate,
                "missing": missing,
            },
            "schedule": {
                "research_utc": e.agent.research_utc,
                "monitor_every_minutes": e.agent.monitor_every_minutes,
            },
            "limits": e.agent.limits,
            "scorecard": card,
        }))
    }

    async fn run(&self, kind: &str, request: Option<String>) -> Result<Value, StoreError> {
        let kind = match kind {
            "research" => RunKind::Research,
            "monitor" => RunKind::Monitor,
            "manual" => RunKind::Manual,
            other => return Err(StoreError(format!("unknown run kind {other}"))),
        };
        let _guard = self
            .runtime
            .stores
            .locks
            .try_acquire("agent-run")
            .await?
            .ok_or_else(|| StoreError("another agent run is in progress".to_owned()))?;
        if let Err(e) = self.evaluate_now().await {
            tracing::warn!(error = %e, "scoring predictions failed");
        }
        let run_id = AgentRunId::new_at(self.runtime.clock.now());
        let tools = self.tools(run_id).await?;
        let record = qd_agent::agent::run(&tools, kind, request.as_deref()).await;
        serde_json::to_value(record).map_err(error)
    }

    async fn runs(&self, limit: i64) -> Result<Value, StoreError> {
        let entries = JournalReader::recent(
            self.runtime.stores.journal.as_ref(),
            Some("agent_run"),
            None,
            limit.clamp(1, 50),
        )
        .await?;
        Ok(Value::Array(entries.into_iter().map(|e| e.entry).collect()))
    }

    async fn predictions(&self) -> Result<Value, StoreError> {
        let (_, predictions, outcomes) =
            Tools::record_of(self.runtime.stores.journal.as_ref(), None)
                .await
                .map_err(StoreError)?;
        Ok(Value::Array(
            predictions
                .iter()
                .rev()
                .take(500)
                .map(|p| {
                    json!({
                        "prediction": p,
                        "outcome": outcomes.iter().find(|o| o.prediction == p.id),
                    })
                })
                .collect(),
        ))
    }

    async fn scorecard(&self) -> Result<Value, StoreError> {
        let (predictions, outcomes, trades) =
            Tools::load_record(self.runtime.stores.journal.as_ref())
                .await
                .map_err(StoreError)?;
        let mut models: Vec<String> = predictions.iter().map(|p| p.model.clone()).collect();
        models.sort();
        models.dedup();
        let per_model: Vec<Value> = models
            .iter()
            .map(|m| {
                let mine: Vec<_> = predictions.iter().filter(|p| &p.model == m).cloned().collect();
                json!({"model": m, "scorecard": qd_agent::predictions::scorecard(&mine, &outcomes, &trades)})
            })
            .collect();
        Ok(json!({
            "overall": qd_agent::predictions::scorecard(&predictions, &outcomes, &trades),
            "models": per_model,
        }))
    }

    async fn evaluate(&self) -> Result<Value, StoreError> {
        self.evaluate_now().await
    }
}

/// Runs the daily research run and the market-hours monitoring runs at the
/// configured times; checks every minute.
pub fn spawn_schedule(
    runtime: Arc<Runtime>,
    agent: Arc<DynAgent>,
    notifier: Arc<crate::notify::TelegramNotifier>,
    clock: Arc<dyn Clock>,
) {
    tokio::spawn(async move {
        let mut research_done: Option<NaiveDate> = None;
        let mut last_monitor: Option<DateTime<Utc>> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let Ok(e) = runtime.effective().await else {
                continue;
            };
            if !e.agent.enabled {
                continue;
            }
            let now = clock.now();
            let today = now.date_naive();
            if let Some(at) = e.agent.research_utc {
                if now.time() >= at && research_done != Some(today) {
                    research_done = Some(today);
                    report(&notifier, "research", agent.run("research", None).await).await;
                }
            }
            if let Some(every) = e.agent.monitor_every_minutes {
                let ist_today = now
                    .with_timezone(&qd_broker_kite::market::ist())
                    .date_naive();
                let trading = runtime
                    .config
                    .calendars
                    .get(&CalendarId("nse".to_owned()))
                    .and_then(|c| c.is_trading_day(ist_today))
                    .unwrap_or(true);
                let open = qd_broker_kite::broker::exchange_open(&Venue::Nse, now) && trading;
                let due = last_monitor
                    .is_none_or(|t| now - t >= chrono::Duration::minutes(i64::from(every)));
                if open && due {
                    last_monitor = Some(now);
                    report(&notifier, "monitoring", agent.run("monitor", None).await).await;
                }
            }
        }
    });
}

async fn report(
    notifier: &crate::notify::TelegramNotifier,
    what: &str,
    result: Result<Value, StoreError>,
) {
    match result {
        Ok(record) => {
            let trades = record
                .get("decisions")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            tracing::info!(run = %record.get("id").cloned().unwrap_or_default(), status = %record.get("status").cloned().unwrap_or_default(), trades, "AI agent {what} run finished");
            if trades > 0 {
                let summary: String = record
                    .get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(1500)
                    .collect();
                notifier
                    .notify_if_enabled(&format!(
                        "QuantDesk AI agent ({what}): {trades} trade request(s).\n{summary}"
                    ))
                    .await;
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "AI agent {what} run did not run");
        }
    }
}
