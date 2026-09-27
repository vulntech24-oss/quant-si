//! `qd`: the QuantDesk admin CLI. It connects with `QD_DATABASE_URL` (never
//! stored in files) and writes every change through the same stores and use
//! cases the server uses, so the audit log and invariants apply.

use std::path::PathBuf;

use chrono::{NaiveDate, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use qd_app::ports::{AuditLog, BacktestRequest, BacktestRunner, HaltStore};
use qd_app::registry::{StrategyRegistry, StrategyVersionRecord};
use qd_backtest::research::ResearchBacktester;
use qd_domain::halt::{ClearedBy, Halt, HaltKind, HaltScope};
use qd_domain::ids::{AccountId, HaltId, InstrumentId, StrategyId, StrategyVersionId, UserId};
use qd_domain::instrument::InstrumentSpec;
use qd_domain::lifecycle::strategy::StageEvent;
use qd_domain::market::{Bar, BarData, BarSeries};
use qd_domain::num::Currency;
use qd_domain::proposal::{AccountMode, StrategyRef};
use qd_risk::config::{RiskConfig, RiskConfigData};
use qd_store::{AccountRecord, Stores};
use qd_strategy::trend_pullback::TrendPullback;
use rust_decimal::Decimal;

#[derive(Parser)]
#[command(name = "qd", about = "QuantDesk admin CLI")]
struct Cli {
    /// PostgreSQL URL. Read from the environment; never put it in a file.
    #[arg(long, env = "QD_DATABASE_URL", hide_env_values = true)]
    database_url: String,
    /// Who is acting, recorded in the audit log.
    #[arg(long, default_value = "owner-cli")]
    actor: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply database migrations.
    Migrate,
    /// Accounts.
    #[command(subcommand)]
    Account(AccountCommand),
    /// API users.
    #[command(subcommand)]
    User(UserCommand),
    /// Instrument specs.
    #[command(subcommand)]
    Instrument(InstrumentCommand),
    /// Daily bars.
    #[command(subcommand)]
    Bars(BarsCommand),
    /// Strategy registry.
    #[command(subcommand)]
    Strategy(StrategyCommand),
    /// Kill switch.
    #[command(subcommand)]
    Halt(HaltCommand),
    /// Latest journal entries.
    Journal {
        /// How many.
        #[arg(long, default_value_t = 20)]
        limit: i64,
    },
    /// Research backtest of trend-pullback on stored bars (neutral evidence prior).
    Backtest(BacktestArgs),
    /// Paper trading with the server's `[paper]` configuration.
    #[command(subcommand)]
    Paper(PaperCommand),
    /// Advisory AI (shadow mode, INV-04) with the server's `[ai]` configuration.
    Ai {
        /// Server configuration file.
        #[arg(long, env = "QD_CONFIG")]
        config: PathBuf,
        #[command(subcommand)]
        command: AiCommand,
    },
}

#[derive(Subcommand)]
enum AiCommand {
    /// Advise on entry decisions not yet advised.
    Run,
    /// Show advice, optionally for one decision.
    Advice {
        #[arg(long)]
        decision: Option<qd_domain::ids::DecisionId>,
    },
    /// How each advisor's stances matched realized outcomes.
    Scorecard,
}

#[derive(Subcommand)]
enum PaperCommand {
    /// Process every trading day after the last processed one, through a date.
    Run {
        /// Server configuration file (the same one `qd-server` uses).
        #[arg(long, env = "QD_CONFIG")]
        config: PathBuf,
        /// Last date to process (default: today, UTC).
        #[arg(long)]
        through: Option<NaiveDate>,
    },
    /// Show the paper book restored from the journal.
    State {
        /// Server configuration file.
        #[arg(long, env = "QD_CONFIG")]
        config: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Paper,
    Live,
}

#[derive(Subcommand)]
enum AccountCommand {
    /// Create an account (never armed for live trading).
    Create {
        #[arg(long)]
        name: String,
        #[arg(long, value_enum)]
        mode: Mode,
        #[arg(long, default_value = "INR")]
        currency: String,
        /// Use this id (the one in the server configuration) instead of a new one.
        #[arg(long)]
        id: Option<AccountId>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum UserRole {
    Owner,
    Viewer,
}

#[derive(Subcommand)]
enum UserCommand {
    /// Create a user. The password is read from standard input, never from
    /// arguments (they are visible to other processes).
    Create {
        #[arg(long)]
        username: String,
        #[arg(long, value_enum)]
        role: UserRole,
    },
}

#[derive(Subcommand)]
enum InstrumentCommand {
    /// Add a spec version from a TOML file (an InstrumentSpecData).
    Add { file: PathBuf },
    /// List stored spec versions.
    List,
}

#[derive(Subcommand)]
enum BarsCommand {
    /// Import daily bars from a CSV with columns date,open,high,low,close,volume.
    Import {
        #[arg(long)]
        instrument: InstrumentId,
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum StrategyCommand {
    /// Register trend-pullback-1.0.0 as a new version (Draft).
    RegisterTrendPullback {
        #[arg(long)]
        version_number: u32,
        #[arg(long)]
        git_sha: String,
        /// Existing strategy family id, if this is a later version.
        #[arg(long)]
        strategy_id: Option<StrategyId>,
    },
    /// List versions and their stages.
    List,
    /// Apply a stage event given as JSON, e.g. '{"event":"start_research"}'.
    Event {
        #[arg(long)]
        version: StrategyVersionId,
        #[arg(long)]
        event: String,
    },
    /// Validate a version (walk-forward, out-of-sample, holdout, Monte Carlo)
    /// and record the evidence. Criteria come from the server configuration.
    Validate {
        /// Server configuration file (for risk, costs and criteria).
        #[arg(long, env = "QD_CONFIG")]
        config: PathBuf,
        #[arg(long)]
        version: StrategyVersionId,
        /// Instruments (repeat); none means every instrument with bars.
        #[arg(long)]
        instrument: Vec<InstrumentId>,
        #[arg(long)]
        from: NaiveDate,
        #[arg(long)]
        to: NaiveDate,
        /// Starting equity of each run.
        #[arg(long, default_value = "1000000")]
        equity: Decimal,
    },
    /// Predicted-vs-realized review of the configured paper account.
    /// With `--record`, records a paper review of that version as evidence.
    Review {
        /// Server configuration file (account and review criteria).
        #[arg(long, env = "QD_CONFIG")]
        config: PathBuf,
        #[arg(long)]
        record: Option<StrategyVersionId>,
    },
    /// Recorded evidence, newest first (summaries).
    Evidence {
        #[arg(long)]
        version: Option<StrategyVersionId>,
    },
}

#[derive(Subcommand)]
enum HaltCommand {
    /// Create a manual global halt (only a human can clear it).
    Create {
        #[arg(long)]
        reason: String,
    },
    /// Clear (re-arm) a halt as a human.
    Clear {
        #[arg(long)]
        id: HaltId,
        #[arg(long)]
        user: UserId,
    },
    /// List halts.
    List,
}

#[derive(clap::Args)]
struct BacktestArgs {
    #[arg(long)]
    instrument: InstrumentId,
    #[arg(long)]
    from: NaiveDate,
    #[arg(long)]
    to: NaiveDate,
    #[arg(long, default_value = "1000000")]
    equity: Decimal,
    /// Risk configuration file.
    #[arg(long, default_value = "config/risk.toml")]
    risk_config: PathBuf,
    /// Cost schedule file.
    #[arg(long, default_value = "config/costs/india-zerodha.toml")]
    costs: PathBuf,
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn print(value: &impl serde::Serialize) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    #[allow(clippy::print_stdout)] // the CLI's output is its interface
    {
        println!("{text}");
    }
    Ok(())
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            #[allow(clippy::print_stderr)] // report the failure to the operator
            {
                eprintln!("error: {e}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), String> {
    let pool = qd_store::connect(&cli.database_url, 4)
        .await
        .map_err(|e| e.to_string())?;
    if matches!(cli.command, Command::Migrate) {
        qd_store::migrate(&pool).await.map_err(|e| e.to_string())?;
        return print(&"migrations applied");
    }
    let stores = Stores::new(&pool);
    let now = Utc::now();
    let actor = cli.actor.as_str();
    match cli.command {
        Command::Migrate => Ok(()),
        Command::Account(AccountCommand::Create {
            name,
            mode,
            currency,
            id,
        }) => {
            Currency::new(&currency).map_err(|e| e.to_string())?;
            let record = AccountRecord {
                id: id.unwrap_or_else(|| AccountId::new_at(now)),
                name,
                mode: match mode {
                    Mode::Paper => AccountMode::Paper,
                    Mode::Live => AccountMode::Live,
                },
                currency,
                live_armed: false,
            };
            stores
                .accounts
                .create(&record, actor)
                .await
                .map_err(|e| e.to_string())?;
            print(&record)
        }
        Command::User(UserCommand::Create { username, role }) => {
            let mut password = String::new();
            std::io::stdin()
                .read_line(&mut password)
                .map_err(|e| e.to_string())?;
            let password = password.trim_end_matches(['\n', '\r']);
            let hash = qd_api::auth::hash_password(password).map_err(|e| e.to_string())?;
            let user = qd_app::ports::UserRecord {
                id: UserId::new_at(now),
                username,
                role: match role {
                    UserRole::Owner => qd_app::ports::Role::Owner,
                    UserRole::Viewer => qd_app::ports::Role::Viewer,
                },
                password_hash: hash,
            };
            qd_app::ports::AuthStore::create_user(stores.auth.as_ref(), &user)
                .await
                .map_err(|e| e.to_string())?;
            print(&serde_json::json!({ "user": user.id, "username": user.username }))
        }
        Command::Instrument(InstrumentCommand::Add { file }) => {
            let data: qd_domain::instrument::InstrumentSpecData = read_toml(&file)?;
            let spec = InstrumentSpec::new(data).map_err(|e| e.to_string())?;
            stores
                .market
                .add_instrument(&spec)
                .await
                .map_err(|e| e.to_string())?;
            stores
                .audit
                .record(
                    actor,
                    "instrument.add",
                    serde_json::json!({ "id": spec.id, "version": spec.version }),
                )
                .await
                .map_err(|e| e.to_string())?;
            print(&spec)
        }
        Command::Instrument(InstrumentCommand::List) => print(
            &stores
                .market
                .all_instruments()
                .await
                .map_err(|e| e.to_string())?,
        ),
        Command::Bars(BarsCommand::Import { instrument, file }) => {
            let mut reader = csv::Reader::from_path(&file).map_err(|e| e.to_string())?;
            let mut bars = Vec::new();
            for record in reader.deserialize::<BarData>() {
                let data = record.map_err(|e| e.to_string())?;
                bars.push(Bar::new(data).map_err(|e| e.to_string())?);
            }
            // Validates order and rejects duplicates before anything is written.
            let last = bars.last().map_or(now.date_naive(), Bar::date);
            let series = BarSeries::new(instrument, last, bars).map_err(|e| e.to_string())?;
            let written = stores
                .market
                .insert_bars(instrument, series.bars(), now)
                .await
                .map_err(|e| e.to_string())?;
            stores
                .audit
                .record(
                    actor,
                    "bars.import",
                    serde_json::json!({ "instrument": instrument, "rows": written }),
                )
                .await
                .map_err(|e| e.to_string())?;
            print(&serde_json::json!({ "rows_written": written }))
        }
        Command::Strategy(cmd) => strategy(cmd, &stores, actor, now).await,
        Command::Halt(cmd) => halt(cmd, &stores, actor, now).await,
        Command::Journal { limit } => print(
            &stores
                .journal
                .recent(limit)
                .await
                .map_err(|e| e.to_string())?,
        ),
        Command::Backtest(args) => backtest(args, &stores).await,
        Command::Paper(cmd) => paper(cmd, &stores, actor, now).await,
        Command::Ai { config, command } => {
            use qd_app::ports::AiAdvisory;
            let service = qd_server::runtime::DynAi(runtime(&config, &stores)?);
            let out = match command {
                AiCommand::Run => {
                    stores
                        .audit
                        .record(actor, "ai.run", serde_json::json!({}))
                        .await
                        .map_err(|e| e.to_string())?;
                    service.run().await
                }
                AiCommand::Advice { decision } => service.advice(decision).await,
                AiCommand::Scorecard => service.scorecard().await,
            }
            .map_err(|e| e.to_string())?;
            print(&out)
        }
    }
}

/// The server runtime for a configuration file: file defaults plus the
/// settings saved in the web UI, exactly as the server uses them.
fn runtime(
    config: &std::path::Path,
    stores: &Stores,
) -> Result<std::sync::Arc<qd_server::runtime::Runtime>, String> {
    let config = qd_server::config::ServerConfig::load(config, &|k| std::env::var(k).ok())
        .map_err(|e| e.to_string())?;
    Ok(std::sync::Arc::new(qd_server::runtime::Runtime::new(
        config,
        stores.clone(),
        std::sync::Arc::new(qd_server::SystemClock),
    )))
}

async fn paper(
    cmd: PaperCommand,
    stores: &Stores,
    actor: &str,
    now: chrono::DateTime<Utc>,
) -> Result<(), String> {
    use qd_app::ports::PaperTrading;
    let path = match &cmd {
        PaperCommand::Run { config, .. } | PaperCommand::State { config } => config.clone(),
    };
    let runner = qd_server::runtime::DynPaper(runtime(&path, stores)?);
    match cmd {
        PaperCommand::Run { through, .. } => {
            let through = through.unwrap_or_else(|| now.date_naive());
            stores
                .audit
                .record(
                    actor,
                    "paper.run",
                    serde_json::json!({ "through": through }),
                )
                .await
                .map_err(|e| e.to_string())?;
            print(
                &runner
                    .run_through(through)
                    .await
                    .map_err(|e| e.to_string())?,
            )
        }
        PaperCommand::State { .. } => print(&runner.state().await.map_err(|e| e.to_string())?),
    }
}

async fn strategy(
    cmd: StrategyCommand,
    stores: &Stores,
    actor: &str,
    now: chrono::DateTime<Utc>,
) -> Result<(), String> {
    let registry = StrategyRegistry::new(
        stores.registry.clone(),
        stores.audit.clone(),
        stores.evidence.clone(),
    );
    match cmd {
        StrategyCommand::RegisterTrendPullback {
            version_number,
            git_sha,
            strategy_id,
        } => {
            let strategy = TrendPullback::v1();
            let record = StrategyVersionRecord {
                reference: StrategyRef {
                    strategy_id: strategy_id.unwrap_or_else(|| StrategyId::new_at(now)),
                    name: "Trend pullback".to_owned(),
                    version_id: StrategyVersionId::new_at(now),
                    version_number,
                    logic_version: TrendPullback::LOGIC_VERSION.to_owned(),
                    git_sha,
                },
                parameters: serde_json::to_value(strategy.params()).map_err(|e| e.to_string())?,
                rr_floor: strategy.params().rr_floor,
            };
            registry
                .register(&record, actor)
                .await
                .map_err(|e| e.to_string())?;
            print(&record)
        }
        StrategyCommand::Validate {
            config,
            version,
            instrument,
            from,
            to,
            equity,
        } => {
            use qd_app::ports::Validator;
            let validator = qd_server::runtime::DynValidator(runtime(&config, stores)?);
            let record = validator
                .validate(
                    &qd_app::ports::ValidationRequest {
                        version,
                        instruments: instrument,
                        from,
                        to,
                        equity,
                    },
                    actor,
                )
                .await
                .map_err(|e| e.to_string())?;
            // The full report includes every trade; print the verdict and checks.
            print(&serde_json::json!({
                "evidence_id": record["id"],
                "passed": record["passed"],
                "checks": record["report"]["checks"],
                "evidence": record["report"]["evidence"],
                "oos": record["report"]["oos"],
                "holdout": record["report"]["holdout"],
                "monte_carlo": record["report"]["monte_carlo"],
            }))
        }
        StrategyCommand::Review { config, record } => {
            use qd_app::review::Reviewer;
            let reviewer = qd_server::runtime::DynReviewer(runtime(&config, stores)?);
            let out = match record {
                Some(version) => reviewer.record(version, actor).await,
                None => reviewer.review().await,
            }
            .map_err(|e| e.to_string())?;
            print(&out)
        }
        StrategyCommand::Evidence { version } => {
            use qd_app::evidence::EvidenceStore;
            let records = stores
                .evidence
                .list(version)
                .await
                .map_err(|e| e.to_string())?;
            let out: Vec<serde_json::Value> = records
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id, "version": r.version, "kind": r.kind,
                        "passed": r.passed, "created_at": r.created_at,
                        "checks": r.report.get("checks"),
                    })
                })
                .collect();
            print(&out)
        }
        StrategyCommand::List => {
            let mut out = Vec::new();
            for v in stores_versions(stores).await? {
                let stage = registry
                    .stage(v.reference.version_id)
                    .await
                    .map_err(|e| e.to_string())?;
                out.push(serde_json::json!({ "version": v, "stage": stage }));
            }
            print(&out)
        }
        StrategyCommand::Event { version, event } => {
            let event: StageEvent = serde_json::from_str(&event).map_err(|e| e.to_string())?;
            let stage = registry
                .transition(version, &event, actor)
                .await
                .map_err(|e| e.to_string())?;
            print(&stage)
        }
    }
}

async fn stores_versions(stores: &Stores) -> Result<Vec<StrategyVersionRecord>, String> {
    use qd_app::registry::StrategyRegistryStore;
    stores.registry.versions().await.map_err(|e| e.to_string())
}

async fn halt(
    cmd: HaltCommand,
    stores: &Stores,
    actor: &str,
    now: chrono::DateTime<Utc>,
) -> Result<(), String> {
    match cmd {
        HaltCommand::Create { reason } => {
            let halt = Halt::new(
                HaltId::new_at(now),
                HaltKind::Manual,
                HaltScope::Global,
                reason,
                now,
                None,
                true,
            )
            .map_err(|e| e.to_string())?;
            stores
                .halts
                .record(&halt)
                .await
                .map_err(|e| e.to_string())?;
            stores
                .audit
                .record(
                    actor,
                    "halt.create",
                    serde_json::json!({ "halt": halt.id() }),
                )
                .await
                .map_err(|e| e.to_string())?;
            print(&halt)
        }
        HaltCommand::Clear { id, user } => {
            let halts = stores.halts.load().await.map_err(|e| e.to_string())?;
            let halt = halts
                .into_iter()
                .find(|h| h.id() == id)
                .ok_or("unknown halt")?;
            let cleared = halt
                .clear(ClearedBy::Human(user), now)
                .map_err(|e| e.to_string())?;
            stores
                .halts
                .record(&cleared)
                .await
                .map_err(|e| e.to_string())?;
            stores
                .audit
                .record(
                    actor,
                    "halt.clear",
                    serde_json::json!({ "halt": id, "user": user }),
                )
                .await
                .map_err(|e| e.to_string())?;
            print(&cleared)
        }
        HaltCommand::List => print(&stores.halts.load().await.map_err(|e| e.to_string())?),
    }
}

async fn backtest(args: BacktestArgs, stores: &Stores) -> Result<(), String> {
    let risk_data: RiskConfigData = read_toml(&args.risk_config)?;
    let risk = RiskConfig::new(risk_data).map_err(|e| e.to_string())?;
    let costs = qd_domain::costs::ScheduleCostModel::new(read_toml(&args.costs)?)
        .map_err(|e| e.to_string())?;
    let runner = ResearchBacktester::new(
        stores.market.clone(),
        std::sync::Arc::new(costs),
        &risk,
        std::sync::Arc::new(qd_server::SystemClock),
    )
    .map_err(|e| e.to_string())?;
    let report = runner
        .run(&BacktestRequest {
            instrument: args.instrument,
            from: args.from,
            to: args.to,
            equity: args.equity,
        })
        .await
        .map_err(|e| e.to_string())?;
    print(&report)
}
