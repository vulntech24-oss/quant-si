//! Paper-trading wiring: the runner over the PostgreSQL stores, and the
//! optional automatic daily run (ADR 0009).

use std::sync::Arc;

use chrono::{NaiveDate, NaiveTime};
use qd_app::ports::{Clock, PaperTrading};
use qd_app::registry::StrategyRegistry;
use qd_broker_paper::{PaperDeps, PaperRunner, PaperSettings};
use qd_store::Stores;

use crate::config::ServerConfig;

/// Builds the paper runner if the configuration has a `[paper]` section.
pub fn paper_runner(
    config: &ServerConfig,
    stores: &Stores,
    clock: Arc<dyn Clock>,
) -> Result<Option<Arc<PaperRunner>>, String> {
    let Some(paper) = &config.file.paper else {
        return Ok(None);
    };
    let deps = PaperDeps {
        journal: stores.journal.clone(),
        reader: stores.journal.clone(),
        halts: stores.halts.clone(),
        market: stores.market.clone(),
        registry: StrategyRegistry::new(
            stores.registry.clone(),
            stores.audit.clone(),
            stores.evidence.clone(),
        ),
        accounts: stores.accounts.clone(),
        costs: Arc::new(config.costs.clone()),
        risk: config.risk.clone(),
        evidence: Arc::new(qd_app::evidence::StoreEvidenceLoader(
            stores.evidence.clone(),
        )),
        lock: stores.locks.clone(),
        clock,
    };
    let settings = PaperSettings {
        account: config.file.account_id,
        initial_equity: paper.initial_equity,
        start: paper.start_date,
        close_time_utc: paper.close_time_utc,
        slippage_ticks: paper.slippage_ticks,
        warm_up_days: paper.warm_up_days,
        calendar_version: "unversioned".to_owned(),
    };
    PaperRunner::new(deps, settings)
        .map(|r| Some(Arc::new(r)))
        .map_err(|e| e.to_string())
}

/// The date a run started at `now` processes through: today once the
/// configured run time has passed.
#[must_use]
pub fn due_date(now: chrono::DateTime<chrono::Utc>, run_at: NaiveTime) -> Option<NaiveDate> {
    (now.time() >= run_at).then(|| now.date_naive())
}

/// Runs paper trading once a day after `run_at` (UTC), then advisory AI if
/// enabled. Checks every minute; the run lock and the journal's last
/// processed day make repeats harmless.
pub fn spawn_daily(
    runner: Arc<PaperRunner>,
    clock: Arc<dyn Clock>,
    run_at: NaiveTime,
    ai: Option<Arc<dyn qd_app::ports::AiAdvisory>>,
) {
    tokio::spawn(async move {
        let mut last: Option<NaiveDate> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let Some(date) = due_date(clock.now(), run_at) else {
                continue;
            };
            if last == Some(date) {
                continue;
            }
            match runner.run_through(date).await {
                Ok(report) => {
                    last = Some(date);
                    tracing::info!(%date, report = %report, "paper run complete");
                    if let Some(ai) = &ai {
                        match ai.run().await {
                            Ok(r) => tracing::info!(report = %r, "advisory AI run complete"),
                            Err(e) => tracing::warn!(error = %e, "advisory AI run failed"),
                        }
                    }
                }
                Err(e) => tracing::error!(%date, error = %e, "paper run failed"),
            }
        }
    });
}
