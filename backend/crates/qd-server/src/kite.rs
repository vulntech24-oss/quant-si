//! The Zerodha connection and live trading in the server (ADR 0014).
//!
//! - [`DynKite`] implements the `BrokerLink` port: "Login with Zerodha",
//!   bar import, and the intraday fill check.
//! - [`DynLive`] runs the live book (`[live]` in the server file) and
//!   reconciles it with Zerodha at startup.
//! - [`spawn_schedules`] runs the bar import, the daily live run and the
//!   intraday fill checks at the configured times.
//!
//! The access token from a login is stored as the `kite_access_token`
//! secret; it is never returned to a browser (INV-15).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use qd_app::ports::{
    BrokerAccountReader, BrokerLink, Clock, PaperTrading, Reconciler, SecretStore, SecretValue,
    StoreError,
};
use qd_broker_kite::broker::{KiteBroker, exchange_open};
use qd_broker_kite::client::KiteError;
use qd_broker_kite::market::{
    completed_through, daily_candles, instruments, kite_ref, token_index,
};
use qd_domain::calendar::{DataIssue, check_bars};
use qd_domain::instrument::{InstrumentKind, InstrumentSpec, Venue};
use qd_domain::market::Bar;
use serde_json::{Value, json};

use crate::notify::TelegramNotifier;
use crate::runtime::Runtime;

/// How long a login's one-time state stays valid.
const LOGIN_STATE_MINUTES: i64 = 10;

/// Pause between candle requests (Kite allows three per second).
const CANDLE_PAUSE: std::time::Duration = std::time::Duration::from_millis(350);

fn error(e: impl std::fmt::Display) -> StoreError {
    StoreError(e.to_string())
}

/// The latest spec of every instrument effective on `date`.
pub(crate) async fn latest_specs(
    rt: &Runtime,
    date: NaiveDate,
) -> Result<Vec<InstrumentSpec>, StoreError> {
    let mut latest: HashMap<_, InstrumentSpec> = HashMap::new();
    for spec in
        qd_app::ports::HistoricalMarketData::instruments(rt.stores.market.as_ref(), date).await?
    {
        if latest
            .get(&spec.id)
            .is_none_or(|s| spec.version > s.version)
        {
            latest.insert(spec.id, spec);
        }
    }
    let mut specs: Vec<_> = latest.into_values().collect();
    specs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    Ok(specs)
}

/// Imports completed daily bars from Kite for every instrument with a
/// `kite` broker reference. Only dates after the last stored bar are
/// written; corrections of stored bars are left to a CSV import.
pub async fn sync_bars(rt: &Runtime) -> Result<Value, StoreError> {
    let e = rt.effective().await?;
    let client = rt.kite_client().await?;
    if !client.logged_in() {
        return Err(StoreError("log in with Zerodha first".to_owned()));
    }
    let now = rt.clock.now();
    let mut tokens: HashMap<(String, String), String> = HashMap::new();
    let mut loaded: Vec<String> = Vec::new();
    let mut report = Vec::new();
    for spec in latest_specs(rt, now.date_naive()).await? {
        let Some(kref) = kite_ref(&spec) else {
            continue;
        };
        let token = match kref.token.clone() {
            Some(t) => t,
            None => {
                if !loaded.contains(&kref.exchange) {
                    let rows = instruments(&client, &kref.exchange).await.map_err(error)?;
                    tokens.extend(token_index(&rows));
                    loaded.push(kref.exchange.clone());
                }
                match tokens.get(&(kref.exchange.clone(), kref.tradingsymbol.clone())) {
                    Some(t) => t.clone(),
                    None => {
                        report.push(json!({
                            "symbol": spec.symbol,
                            "error": format!("{}:{} is not in Kite's instrument list", kref.exchange, kref.tradingsymbol),
                        }));
                        continue;
                    }
                }
            }
        };
        match import_spec_bars(rt, &e, &client, &spec, &token, e.kite.history_days).await {
            Ok(v) => report.push(v),
            Err(KiteError::Token(m)) => return Err(StoreError(format!("Kite session: {m}"))),
            Err(err) => report.push(json!({ "symbol": spec.symbol, "error": err.to_string() })),
        }
        tokio::time::sleep(CANDLE_PAUSE).await;
    }
    Ok(json!({ "instruments": report }))
}

/// Imports completed daily bars from Kite for one instrument: dates after
/// the last stored bar, within `history_days`, through the data-quality
/// checks (a suspect jump holds back that bar and every later one when
/// `data.hold_suspect_bars` is on).
pub(crate) async fn import_spec_bars(
    rt: &Runtime,
    e: &crate::runtime::Effective,
    client: &qd_broker_kite::client::KiteClient,
    spec: &InstrumentSpec,
    token: &str,
    history_days: i64,
) -> Result<Value, KiteError> {
    let unexpected = |err: StoreError| KiteError::Unexpected(err.0);
    let now = rt.clock.now();
    let to = completed_through(now, &spec.venue);
    let window_start = to - Duration::days(history_days);
    let stored = qd_app::ports::HistoricalMarketData::daily_bars(
        rt.stores.market.as_ref(),
        spec.id,
        window_start,
        to,
        now,
    )
    .await
    .map_err(unexpected)?;
    let from = stored
        .last()
        .map(Bar::date)
        .and_then(|d| d.succ_opt())
        .unwrap_or(window_start);
    if from > to {
        return Ok(json!({ "symbol": spec.symbol, "inserted": 0, "through": to }));
    }
    let continuous = spec.kind == InstrumentKind::Future;
    let bars = daily_candles(client, token, from, to, continuous, to).await?;
    let mut new: Vec<Bar> = bars.into_iter().filter(|b| b.date() >= from).collect();
    let issues = check_bars(
        stored.last(),
        &new,
        rt.config.calendars.get(&spec.calendar_id),
        e.data.limits(),
    );
    let first_jump = issues.iter().find_map(|i| match i {
        DataIssue::PriceJump { date, .. } => Some(*date),
        _ => None,
    });
    let mut held_back = 0;
    if let (true, Some(jump)) = (e.data.hold_suspect_bars, first_jump) {
        let before = new.len();
        new.retain(|b| b.date() < jump);
        held_back = before - new.len();
    }
    let inserted = rt
        .stores
        .market
        .insert_bars(spec.id, &new, now)
        .await
        .map_err(unexpected)?;
    let notable: Vec<&DataIssue> = issues
        .iter()
        .filter(|i| !matches!(i, DataIssue::CalendarUnknown { .. }))
        .collect();
    Ok(json!({
        "symbol": spec.symbol,
        "inserted": inserted,
        "through": to,
        "held_back": held_back,
        "issues": notable,
    }))
}

/// The Zerodha connection behind the `BrokerLink` port.
pub struct DynKite {
    runtime: Arc<Runtime>,
    live: Arc<DynLive>,
    login_states: Mutex<HashMap<String, DateTime<Utc>>>,
    last_login: Mutex<Option<(String, DateTime<Utc>)>>,
}

impl std::fmt::Debug for DynKite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynKite").finish_non_exhaustive()
    }
}

impl DynKite {
    /// The connection for a runtime.
    #[must_use]
    pub fn new(runtime: Arc<Runtime>, live: Arc<DynLive>) -> Self {
        Self {
            runtime,
            live,
            login_states: Mutex::new(HashMap::new()),
            last_login: Mutex::new(None),
        }
    }

    fn states(&self) -> std::sync::MutexGuard<'_, HashMap<String, DateTime<Utc>>> {
        self.login_states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl BrokerLink for DynKite {
    async fn status(&self) -> Result<Value, StoreError> {
        let e = self.runtime.effective().await?;
        let set = |name: &'static str| async move { self.runtime.secret(name).await.is_some() };
        let last_login = self
            .last_login
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        Ok(json!({
            "enabled": e.kite.enabled,
            "user_id": e.kite.user_id,
            "api_key_set": set("kite_api_key").await,
            "api_secret_set": set("kite_api_secret").await,
            "access_token_set": set("kite_access_token").await,
            "last_login": last_login.map(|(user, at)| json!({ "user_id": user, "at": at })),
            "live_configured": self.runtime.config.file.live.is_some(),
            "live_trading_enabled": self.runtime.config.live.live_trading_enabled,
            "live_orders_compiled": qd_app::live::live_orders_compiled(),
            "redirect_path": "/api/kite/callback",
        }))
    }

    async fn login_url(&self, actor: &str) -> Result<String, StoreError> {
        let client = self.runtime.kite_client().await?;
        if self.runtime.secret("kite_api_secret").await.is_none() {
            return Err(StoreError(
                "the Kite API secret is not set (Settings → API keys)".to_owned(),
            ));
        }
        let mut raw = [0_u8; 24];
        getrandom::fill(&mut raw).map_err(error)?;
        let state = hex::encode(raw);
        let now = self.runtime.clock.now();
        {
            let mut states = self.states();
            states.retain(|_, expires| *expires > now);
            states.insert(state.clone(), now + Duration::minutes(LOGIN_STATE_MINUTES));
        }
        qd_app::ports::AuditLog::record(
            self.runtime.stores.audit.as_ref(),
            actor,
            "kite.login_started",
            json!({}),
        )
        .await?;
        Ok(qd_broker_kite::login::login_url(client.api_key(), &state))
    }

    async fn complete_login(&self, state: &str, request_token: &str) -> Result<Value, StoreError> {
        let now = self.runtime.clock.now();
        let valid = self
            .states()
            .remove(state)
            .is_some_and(|expires| expires > now);
        if !valid {
            return Err(StoreError("unknown or expired login state".to_owned()));
        }
        if request_token.is_empty()
            || request_token.len() > 128
            || !request_token.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(StoreError("malformed request token".to_owned()));
        }
        let e = self.runtime.effective().await?;
        let client = self.runtime.kite_client().await?;
        let secret = self
            .runtime
            .secret("kite_api_secret")
            .await
            .ok_or_else(|| StoreError("the Kite API secret is not set".to_owned()))?;
        let session = qd_broker_kite::login::create_session(&client, request_token, &secret)
            .await
            .map_err(error)?;
        if !session.user_id.eq_ignore_ascii_case(&e.kite.user_id) {
            return Err(StoreError(format!(
                "logged in as {}, but Settings expect {}; token not stored",
                session.user_id, e.kite.user_id
            )));
        }
        self.runtime
            .secrets
            .set(
                "kite_access_token",
                &SecretValue::new(session.access_token.clone()),
                "kite-login",
            )
            .await?;
        *self
            .last_login
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((session.user_id.clone(), now));
        qd_app::ports::AuditLog::record(
            self.runtime.stores.audit.as_ref(),
            "kite-login",
            "kite.login",
            json!({ "user_id": session.user_id }),
        )
        .await?;
        Ok(json!({ "user_id": session.user_id }))
    }

    async fn sync_bars(&self) -> Result<Value, StoreError> {
        sync_bars(&self.runtime).await
    }

    async fn sync_fills(&self) -> Result<Value, StoreError> {
        self.live.sync().await
    }
}

/// The live book, when `[live]` is configured.
pub struct DynLive(pub Arc<Runtime>);

const LIVE_OFF: &str = "live trading is not configured ([live] in the server file)";

impl DynLive {
    /// Applies Kite fills now.
    pub async fn sync(&self) -> Result<Value, StoreError> {
        let runner = self
            .0
            .live_runner()
            .await?
            .ok_or_else(|| StoreError(LIVE_OFF.to_owned()))?;
        let e = self.0.effective().await?;
        let client = self.0.kite_client().await?;
        let report = runner.sync(client, e.kite.orders()).await.map_err(error)?;
        serde_json::to_value(report).map_err(error)
    }
}

#[async_trait]
impl PaperTrading for DynLive {
    async fn run_through(&self, through: NaiveDate) -> Result<Value, StoreError> {
        let runner = self
            .0
            .live_runner()
            .await?
            .ok_or_else(|| StoreError(LIVE_OFF.to_owned()))?;
        let e = self.0.effective().await?;
        let client = self.0.kite_client().await?;
        let report = runner
            .run(client, e.kite.orders(), through)
            .await
            .map_err(error)?;
        serde_json::to_value(report).map_err(error)
    }

    async fn state(&self) -> Result<Value, StoreError> {
        let (Some(live), Some(runner)) = (&self.0.config.file.live, self.0.live_runner().await?)
        else {
            return Ok(json!({ "configured": false }));
        };
        let state = runner.state().await.map_err(error)?;
        Ok(qd_app::runs::book_json(live.account_id, &state))
    }
}

#[async_trait]
impl Reconciler for DynLive {
    /// Compares the live book with Zerodha's positions. Without a live book
    /// there is nothing to reconcile; without a usable Kite session the book
    /// cannot be checked, so entries stay halted (INV-07).
    async fn reconcile(&self) -> Result<Vec<String>, StoreError> {
        let (Some(_), Some(runner)) = (&self.0.config.file.live, self.0.live_runner().await?)
        else {
            return Ok(Vec::new());
        };
        let state = match runner.state().await {
            Ok(s) => s,
            Err(e) => return Ok(vec![format!("live book: {e}")]),
        };
        if let Err(e) = state.check() {
            return Ok(vec![format!("live book: {e}")]);
        }
        let client = match self.0.kite_client().await {
            Ok(c) if c.logged_in() => c,
            Ok(_) => return Ok(vec!["live book: not logged in with Zerodha".to_owned()]),
            Err(e) => return Ok(vec![format!("live book: {e}")]),
        };
        let e = self.0.effective().await?;
        let specs = latest_specs(&self.0, self.0.clock.now().date_naive()).await?;
        let broker = KiteBroker::new(client, e.kite.orders(), self.0.clock.clone(), &specs);
        let positions = match broker.positions().await {
            Ok(p) => p,
            Err(e) => {
                return Ok(vec![format!(
                    "live book: Zerodha positions unreadable: {e}"
                )]);
            }
        };
        Ok(
            qd_app::positions::reconcile_book(&state.active_positions(), &positions)
                .into_iter()
                .map(|m| {
                    format!(
                        "live instrument {}: book {} vs Zerodha {}",
                        m.instrument, m.book, m.broker
                    )
                })
                .collect(),
        )
    }
}

/// Every reconciler must pass (paper and live).
pub struct AllReconcilers(pub Vec<Arc<dyn Reconciler>>);

#[async_trait]
impl Reconciler for AllReconcilers {
    async fn reconcile(&self) -> Result<Vec<String>, StoreError> {
        let mut problems = Vec::new();
        for r in &self.0 {
            problems.extend(r.reconcile().await?);
        }
        Ok(problems)
    }
}

fn summary(title: &str, report: &Value) -> String {
    let mut text = format!("QuantDesk {title}");
    for key in [
        "processed",
        "decisions",
        "unprotected",
        "mismatches",
        "demoted",
    ] {
        if let Some(v) = report.get(key).filter(|v| !v.is_null()) {
            text.push_str(&format!("\n{key}: {v}"));
        }
    }
    text
}

/// Runs the bar import, the daily live run and the intraday fill checks.
/// Times are read from the effective settings every minute.
pub fn spawn_schedules(
    runtime: Arc<Runtime>,
    kite: Arc<DynKite>,
    live: Arc<DynLive>,
    notifier: Arc<TelegramNotifier>,
    clock: Arc<dyn Clock>,
) {
    tokio::spawn(async move {
        let mut bars_done: Option<NaiveDate> = None;
        let mut live_done: Option<NaiveDate> = None;
        let mut last_fills: Option<DateTime<Utc>> = None;
        let mut warned: Option<NaiveDate> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let Ok(e) = runtime.effective().await else {
                continue;
            };
            if !e.kite.enabled {
                continue;
            }
            let now = clock.now();
            let today = now.date_naive();
            if let Some(at) = e.kite.sync_bars_utc {
                if now.time() >= at && bars_done != Some(today) {
                    bars_done = Some(today);
                    match kite.sync_bars().await {
                        Ok(r) => {
                            tracing::info!(report = %r, "Kite bar import complete");
                            let flagged: Vec<String> = r
                                .get("instruments")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter(|i| {
                                    i.get("issues")
                                        .and_then(Value::as_array)
                                        .is_some_and(|a| !a.is_empty())
                                })
                                .filter_map(|i| {
                                    i.get("symbol").and_then(Value::as_str).map(str::to_owned)
                                })
                                .collect();
                            if !flagged.is_empty() {
                                notifier
                                    .notify_if_enabled(&format!(
                                        "QuantDesk: data issues in today's bars for {}; see Data in the web UI.",
                                        flagged.join(", ")
                                    ))
                                    .await;
                            }
                        }
                        Err(err) => {
                            tracing::error!(error = %err, "Kite bar import failed");
                            notifier
                                .notify_if_enabled(&format!(
                                    "QuantDesk: Kite bar import failed: {err}"
                                ))
                                .await;
                        }
                    }
                }
            }
            let Some(live_config) = &runtime.config.file.live else {
                continue;
            };
            if let Some(at) = live_config.daily_run_utc {
                if now.time() >= at && live_done != Some(today) {
                    live_done = Some(today);
                    match live.run_through(today).await {
                        Ok(r) => {
                            tracing::info!(report = %r, "live run complete");
                            let notes: Vec<&str> = r
                                .pointer("/refresh/fill_notes")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .collect();
                            if !notes.is_empty() {
                                notifier
                                    .notify_if_enabled(&format!(
                                        "QuantDesk fills at Zerodha:\n{}",
                                        notes.join("\n")
                                    ))
                                    .await;
                            }
                            if e.notifications.daily_summary {
                                notifier.notify_if_enabled(&summary("live run", &r)).await;
                            }
                        }
                        Err(err) => {
                            tracing::error!(error = %err, "live run failed");
                            notifier
                                .notify_if_enabled(&format!("QuantDesk: live run failed: {err}"))
                                .await;
                        }
                    }
                }
            }
            // Skip exchange holidays; an uncovered date counts as open.
            let ist_today = now
                .with_timezone(&qd_broker_kite::market::ist())
                .date_naive();
            let trading = |id: &str| {
                runtime
                    .config
                    .calendars
                    .get(&qd_domain::instrument::CalendarId(id.to_owned()))
                    .and_then(|c| c.is_trading_day(ist_today))
                    .unwrap_or(true)
            };
            let open = (exchange_open(&Venue::Nse, now) && trading("nse"))
                || (exchange_open(&Venue::Mcx, now) && trading("mcx"));
            let due = last_fills
                .is_none_or(|t| now - t >= Duration::minutes(i64::from(e.kite.sync_fills_minutes)));
            if open && due {
                last_fills = Some(now);
                match live.sync().await {
                    Ok(r) => {
                        tracing::info!(report = %r, "live fill check complete");
                        let notes: Vec<&str> = r
                            .pointer("/refresh/fill_notes")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .collect();
                        if !notes.is_empty() {
                            notifier
                                .notify_if_enabled(&format!(
                                    "QuantDesk fills at Zerodha:\n{}",
                                    notes.join("\n")
                                ))
                                .await;
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "live fill check failed");
                        if warned != Some(today) {
                            warned = Some(today);
                            notifier
                                .notify_if_enabled(&format!(
                                    "QuantDesk: live fill checks are failing: {err}"
                                ))
                                .await;
                        }
                    }
                }
            }
        }
    });
}
