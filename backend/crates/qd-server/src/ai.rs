//! Advisory AI wiring (ADR 0011): the orchestrator's only write capability
//! is the journal; it is never given the Order Gateway, the Position
//! Manager, the Risk Gate, the registry, accounts, halts or credentials (INV-04).

use std::sync::Arc;

use qd_ai::checklist::ChecklistAdvisor;
use qd_ai::orchestrator::{AiOrchestrator, AiSettings};
use qd_ai::service::AiService;
use qd_app::ports::Clock;
use qd_store::Stores;

use crate::config::ServerConfig;

/// The advisory service if `[ai] enabled = true`.
#[must_use]
pub fn ai_service(
    config: &ServerConfig,
    stores: &Stores,
    clock: Arc<dyn Clock>,
) -> Option<Arc<AiService>> {
    if !config.file.ai.enabled {
        return None;
    }
    let advisors: Vec<Arc<dyn qd_ai::advisor::Advisor>> = vec![Arc::new(ChecklistAdvisor::new(
        config.file.ai.checklist.clone(),
    ))];
    Some(Arc::new(AiService {
        orchestrator: AiOrchestrator::new(
            advisors,
            stores.journal.clone(),
            stores.journal.clone(),
            clock,
            AiSettings {
                max_calls_per_day: config.file.ai.max_calls_per_day,
                timeout_seconds: config.file.ai.timeout_seconds,
            },
        ),
        reader: stores.journal.clone(),
    }))
}
