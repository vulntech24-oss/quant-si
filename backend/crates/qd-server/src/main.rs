//! `qd-server`: loads configuration, migrates, runs the conservative startup
//! sequence and serves HTTP until interrupted.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use qd_server::config::ServerConfig;
use qd_server::http::{HealthState, router};
use qd_server::startup::startup;
use qd_store::Stores;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .json()
        .init();
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "qd-server stopped");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let path = std::env::var("QD_CONFIG")
        .map_or_else(|_| PathBuf::from("config/quantdesk.toml"), PathBuf::from);
    let config =
        ServerConfig::load(&path, &|k| std::env::var(k).ok()).map_err(|e| e.to_string())?;
    tracing::info!(
        environment = ?config.file.environment,
        live_trading_enabled = config.live.live_trading_enabled,
        live_orders_compiled = qd_app::live::live_orders_compiled(),
        bind = %config.file.bind,
        "configuration loaded"
    );
    let pool = qd_store::connect(
        config.database_url.expose(),
        config.file.database_max_connections,
    )
    .await
    .map_err(|e| e.to_string())?;
    qd_store::migrate(&pool).await.map_err(|e| e.to_string())?;
    let stores = Stores::new(&pool);

    // No broker adapter is wired yet (Phase 6), so entries stay halted.
    let report = startup(&pool, stores.halts.as_ref(), None, Utc::now())
        .await
        .map_err(|e| e.to_string())?;
    for check in &report.checks {
        tracing::info!(check = check.name, ok = check.ok, detail = %check.detail, "startup check");
    }
    tracing::info!(
        startup_halt_cleared = report.startup_halt_cleared,
        "startup complete"
    );

    let app = router(HealthState {
        pool,
        halts: Arc::clone(&stores.halts) as Arc<dyn qd_app::ports::HaltStore>,
    });
    let listener = tokio::net::TcpListener::bind(&config.file.bind)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!(bind = %config.file.bind, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}
