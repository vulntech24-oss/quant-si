//! The advisory service behind the `AiAdvisory` port.

use std::sync::Arc;

use async_trait::async_trait;
use qd_app::ports::{AiAdvisory, JournalReader, StoreError};
use qd_app::session::DayRecord;
use qd_domain::ids::DecisionId;
use serde_json::Value;

use crate::orchestrator::{AiOrchestrator, load};
use crate::scorecard::scorecard;

/// Orchestrator plus journal reads.
pub struct AiService {
    /// The orchestrator.
    pub orchestrator: AiOrchestrator,
    /// Journal reads.
    pub reader: Arc<dyn JournalReader>,
}

impl std::fmt::Debug for AiService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiService")
            .field("orchestrator", &self.orchestrator)
            .finish_non_exhaustive()
    }
}

fn json(v: impl serde::Serialize) -> Result<Value, StoreError> {
    serde_json::to_value(v).map_err(|e| StoreError(e.to_string()))
}

#[async_trait]
impl AiAdvisory for AiService {
    async fn run(&self) -> Result<Value, StoreError> {
        json(self.orchestrator.run().await?)
    }

    async fn advice(&self, decision: Option<DecisionId>) -> Result<Value, StoreError> {
        let (_, mut advice) = load(self.reader.as_ref()).await?;
        advice.retain(|a| decision.is_none_or(|d| a.decision == d));
        advice.reverse();
        json(advice)
    }

    async fn scorecard(&self) -> Result<Value, StoreError> {
        let (_, advice) = load(self.reader.as_ref()).await?;
        let mut trades = Vec::new();
        let mut after = 0;
        loop {
            let page = self.reader.replay(&["day_closed"], after, 1000).await?;
            let Some(last) = page.last() else { break };
            after = last.seq;
            let full = page.len() >= 1000;
            for e in page {
                if let Ok(d) = serde_json::from_value::<DayRecord>(e.entry) {
                    trades.extend(d.trades);
                }
            }
            if !full {
                break;
            }
        }
        json(scorecard(&advice, &trades))
    }
}
