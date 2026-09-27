//! The automatic daily paper run (ADR 0009, 0013). The run time is read
//! from the effective settings every minute, so a change in the web UI
//! applies without a restart.

use std::sync::Arc;

use chrono::{NaiveDate, NaiveTime};
use qd_app::ports::{AiAdvisory, Clock, PaperTrading};

use crate::runtime::Runtime;

/// The date a run started at `now` processes through: today once the
/// configured run time has passed.
#[must_use]
pub fn due_date(now: chrono::DateTime<chrono::Utc>, run_at: NaiveTime) -> Option<NaiveDate> {
    (now.time() >= run_at).then(|| now.date_naive())
}

/// Runs paper trading once a day after the configured time (UTC), then
/// advisory AI. Checks every minute; the run lock and the journal's last
/// processed day make repeats harmless.
pub fn spawn_daily(
    runtime: Arc<Runtime>,
    paper: Arc<dyn PaperTrading>,
    ai: Arc<dyn AiAdvisory>,
    notifier: Arc<crate::notify::TelegramNotifier>,
    clock: Arc<dyn Clock>,
) {
    tokio::spawn(async move {
        let mut last: Option<NaiveDate> = None;
        // A failing run is retried every minute; tell the owner once a day.
        let mut warned: Option<NaiveDate> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let Some(run_at) = runtime.daily_run_utc().await else {
                continue;
            };
            let Some(date) = due_date(clock.now(), run_at) else {
                continue;
            };
            if last == Some(date) {
                continue;
            }
            match paper.run_through(date).await {
                Ok(report) => {
                    last = Some(date);
                    tracing::info!(%date, report = %report, "paper run complete");
                    let summary_on = runtime
                        .effective()
                        .await
                        .is_ok_and(|e| e.notifications.daily_summary);
                    if summary_on {
                        let days = report
                            .get("days")
                            .and_then(serde_json::Value::as_array)
                            .map_or(0, Vec::len);
                        let active = report.get("active_positions").cloned().unwrap_or_default();
                        notifier
                            .notify_if_enabled(&format!(
                                "QuantDesk paper run through {date}: {days} day(s) processed, {active} active position(s)."
                            ))
                            .await;
                    }
                    match ai.run().await {
                        Ok(r) => tracing::info!(report = %r, "advisory AI run complete"),
                        Err(e) => tracing::info!(reason = %e, "advisory AI did not run"),
                    }
                }
                Err(e) => {
                    tracing::error!(%date, error = %e, "paper run failed");
                    if warned != Some(date) {
                        warned = Some(date);
                        notifier
                            .notify_if_enabled(&format!("QuantDesk: paper run failed: {e}"))
                            .await;
                    }
                }
            }
        }
    });
}
