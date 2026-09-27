//! QuantDesk AI orchestration (spec §5.3 `qd-ai`), advisory only (INV-04).
//!
//! AI output is commentary on decisions that were already made, post-risk.
//! It is journaled as `ai_advice` entries (shadow mode) and scored later
//! against outcomes. Nothing that places orders, sizes positions, sets
//! limits, changes parameters, holds credentials or controls halts reads
//! it, and this crate cannot reach any of those: its only write is a
//! journal append of advice.
//!
//! - [`advisor`]: the `Advisor` trait, the input packet built from a
//!   journaled decision, and validation of what an advisor returns.
//! - [`checklist`]: a deterministic checklist advisor (no network).
//! - [`orchestrator`]: runs advisors over new entry decisions with a daily
//!   call budget and a timeout per call.
//! - [`scorecard`]: how each advisor's stances matched realized outcomes.
//! - [`service`]: the `AiAdvisory` port for the API and the CLI.
//!
//! Provider adapters (OpenAI, Gemini, xAI) are not written yet: their
//! documentation was unreachable from the build environment
//! (`docs/integrations/ai-providers.md`).

pub mod advisor;
pub mod checklist;
pub mod orchestrator;
pub mod scorecard;
pub mod service;
