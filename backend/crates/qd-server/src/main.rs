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

/// `qd-server healthcheck`: exits 0 if `/health` answers 200 on
/// `QD_HEALTH_ADDR` (default 127.0.0.1:8080). For container healthchecks
/// without curl in the image.
fn healthcheck() -> std::process::ExitCode {
    use std::io::{Read, Write};
    let addr = std::env::var("QD_HEALTH_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let timeout = std::time::Duration::from_secs(3);
    let ok = addr
        .parse::<std::net::SocketAddr>()
        .ok()
        .and_then(|a| std::net::TcpStream::connect_timeout(&a, timeout).ok())
        .and_then(|mut stream| {
            stream.set_read_timeout(Some(timeout)).ok()?;
            stream
                .write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .ok()?;
            let mut head = [0_u8; 12];
            stream.read_exact(&mut head).ok()?;
            Some(head.ends_with(b" 200"))
        })
        .unwrap_or(false);
    if ok {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

fn main() -> std::process::ExitCode {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck();
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(serve()),
        Err(e) => {
            eprintln!("cannot start the runtime: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn serve() -> std::process::ExitCode {
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
    let clock: Arc<dyn qd_app::ports::Clock> = Arc::new(qd_server::SystemClock);
    tracing::info!(
        secrets_available = config.master_key.is_some(),
        "secrets store (set QD_MASTER_KEY or data_dir to enable)"
    );
    let secrets = Arc::new(qd_store::settings::PgSecrets::new(
        pool.clone(),
        config.master_key.clone(),
        stores.audit.clone(),
    ));
    let runtime = Arc::new(qd_server::runtime::Runtime {
        config: config.clone(),
        stores: stores.clone(),
        clock: clock.clone(),
    });
    // Settings saved in the web UI are validated on every read; a bad one
    // stops the server here rather than failing later.
    runtime.effective().await.map_err(|e| e.to_string())?;
    let paper = Arc::new(qd_server::runtime::DynPaper(runtime.clone()));
    let ai: Arc<dyn qd_app::ports::AiAdvisory> =
        Arc::new(qd_server::runtime::DynAi(runtime.clone()));

    // Reconciliation is against the paper venue when paper trading is on;
    // with no venue at all, entries stay halted (INV-07).
    let report = startup(
        &pool,
        stores.halts.as_ref(),
        Some(paper.as_ref() as &dyn qd_app::ports::Reconciler),
        Utc::now(),
    )
    .await
    .map_err(|e| e.to_string())?;
    for check in &report.checks {
        tracing::info!(check = check.name, ok = check.ok, detail = %check.detail, "startup check");
    }
    tracing::info!(
        startup_halt_cleared = report.startup_halt_cleared,
        "startup complete"
    );
    let paper_port = paper.clone() as Arc<dyn qd_app::ports::PaperTrading>;
    qd_server::paper::spawn_daily(
        runtime.clone(),
        paper_port.clone(),
        ai.clone(),
        clock.clone(),
    );

    let registry = qd_app::registry::StrategyRegistry::new(
        stores.registry.clone(),
        stores.audit.clone(),
        stores.evidence.clone(),
    );
    let api_state = qd_api::ApiState {
        auth: stores.auth.clone(),
        journal: stores.journal.clone(),
        halts: stores.halts.clone(),
        registry,
        market: stores.market.clone(),
        accounts: stores.accounts.clone(),
        audit: stores.audit.clone(),
        backtests: Arc::new(qd_server::runtime::DynBacktests(runtime.clone())),
        paper: Some(paper_port.clone()),
        validator: Arc::new(qd_server::runtime::DynValidator(runtime.clone())),
        ai: Some(ai),
        reviewer: Arc::new(qd_server::runtime::DynReviewer(runtime.clone())),
        evidence: stores.evidence.clone(),
        settings_admin: runtime.clone(),
        secrets,
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
    let health = HealthState {
        pool,
        halts: Arc::clone(&stores.halts) as Arc<dyn qd_app::ports::HaltStore>,
        paper: Some(paper_port),
    };
    qd_server::http::spawn_alert_log(health.clone(), std::time::Duration::from_secs(300));
    let mut app = qd_api::router(api_state).merge(router(health));
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
