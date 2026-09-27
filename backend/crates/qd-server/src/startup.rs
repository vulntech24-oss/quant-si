//! Conservative startup (INV-07).
//!
//! 1. Record a global `Startup` halt: entries are halted from the first moment.
//! 2. Run health checks (database, halt store readable).
//! 3. Reconcile with the broker. Without a configured broker there is nothing
//!    to reconcile against, so entries stay halted.
//! 4. Only if every check passed, the system clears the startup halt. Hard
//!    halts from before the restart are untouched: only a human re-arms them.

use chrono::{DateTime, Utc};
use qd_app::ports::{BrokerAccountReader, HaltStore};
use qd_domain::halt::{ClearedBy, Halt, HaltKind, HaltScope};
use qd_domain::ids::HaltId;
use serde::Serialize;
use sqlx::PgPool;

/// One health check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HealthCheck {
    /// Name.
    pub name: &'static str,
    /// Passed.
    pub ok: bool,
    /// Detail.
    pub detail: String,
}

/// The startup outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StartupReport {
    /// The startup halt recorded at boot.
    pub startup_halt: HaltId,
    /// Checks run.
    pub checks: Vec<HealthCheck>,
    /// Whether the startup halt was cleared (other halts may still apply).
    pub startup_halt_cleared: bool,
}

/// Database reachability.
pub async fn check_database(pool: &PgPool) -> HealthCheck {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(pool)
        .await
    {
        Ok(_) => HealthCheck {
            name: "database",
            ok: true,
            detail: "reachable".to_owned(),
        },
        Err(e) => HealthCheck {
            name: "database",
            ok: false,
            detail: e.to_string(),
        },
    }
}

/// Runs the startup sequence.
pub async fn startup(
    pool: &PgPool,
    halts: &dyn HaltStore,
    broker: Option<&dyn BrokerAccountReader>,
    now: DateTime<Utc>,
) -> Result<StartupReport, qd_app::ports::StoreError> {
    let halt = Halt::new(
        HaltId::new_at(now),
        HaltKind::Startup,
        HaltScope::Global,
        "startup: awaiting health checks and broker reconciliation",
        now,
        None,
        false,
    )
    .map_err(|e| qd_app::ports::StoreError(e.to_string()))?;
    halts.record(&halt).await?;

    let mut checks = vec![check_database(pool).await];
    checks.push(match halts.load().await {
        Ok(all) => HealthCheck {
            name: "halt_store",
            ok: true,
            detail: format!("{} halts on record", all.len()),
        },
        Err(e) => HealthCheck {
            name: "halt_store",
            ok: false,
            detail: e.to_string(),
        },
    });
    checks.push(match broker {
        None => HealthCheck {
            name: "broker_reconciliation",
            ok: false,
            detail: "no broker configured; entries stay halted".to_owned(),
        },
        Some(reader) => match reader.positions().await {
            // The server holds no positions of its own at boot until the
            // position book is persisted (Phase 6); any broker position is a
            // mismatch that a human must look at.
            Ok(positions) if positions.is_empty() => HealthCheck {
                name: "broker_reconciliation",
                ok: true,
                detail: "broker reports no positions".to_owned(),
            },
            Ok(positions) => HealthCheck {
                name: "broker_reconciliation",
                ok: false,
                detail: format!("broker reports {} unreconciled positions", positions.len()),
            },
            Err(e) => HealthCheck {
                name: "broker_reconciliation",
                ok: false,
                detail: e.to_string(),
            },
        },
    });

    let all_ok = checks.iter().all(|c| c.ok);
    if all_ok {
        let cleared = halt
            .clear(ClearedBy::System, now)
            .map_err(|e| qd_app::ports::StoreError(e.to_string()))?;
        halts.record(&cleared).await?;
    }
    Ok(StartupReport {
        startup_halt: halt.id(),
        checks,
        startup_halt_cleared: all_ok,
    })
}
