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

    let clock: Arc<dyn qd_app::ports::Clock> = Arc::new(qd_server::SystemClock);
    let backtests = qd_backtest::research::ResearchBacktester::new(
        stores.market.clone(),
        Arc::new(config.costs.clone()),
        &config.risk,
        clock.clone(),
    )
    .map_err(|e| e.to_string())?;
    let api_state = qd_api::ApiState {
        auth: stores.auth.clone(),
        journal: stores.journal.clone(),
        halts: stores.halts.clone(),
        registry: qd_app::registry::StrategyRegistry::new(
            stores.registry.clone(),
            stores.audit.clone(),
        ),
        market: stores.market.clone(),
        accounts: stores.accounts.clone(),
        audit: stores.audit.clone(),
        backtests: Arc::new(backtests),
        clock,
        settings: qd_api::ApiSettings {
            account_id: config.file.account_id,
            environment: config.file.environment,
            live_trading_enabled: config.live.live_trading_enabled,
            live_orders_compiled: qd_app::live::live_orders_compiled(),
            secure_cookies: config.file.environment == qd_app::live::Environment::Production,
            session_hours: config.file.session_hours,
        },
        limiter: Arc::new(qd_api::auth::LoginLimiter::default()),
    };
    let mut app = qd_api::router(api_state).merge(router(HealthState {
        pool,
        halts: Arc::clone(&stores.halts) as Arc<dyn qd_app::ports::HaltStore>,
    }));
    if let Some(dir) = &config.file.frontend_dir {
        let dir = config.base_dir.join(dir);
        tracing::info!(dir = %dir.display(), "serving frontend");
        let index = dir.join("index.html");
        app = app.fallback_service(
            tower_http::services::ServeDir::new(dir)
                .fallback(tower_http::services::ServeFile::new(index)),
        );
    }
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
