//! The QuantDesk AI agent (ADR 0016): the desk's central intelligence.
//!
//! The agent researches the market on the web, finds instruments, reads
//! real-time quotes and daily candles, analyses them, decides whether to
//! trade and how much capital to allocate, monitors its positions, and is
//! scored on every prediction it makes.
//!
//! QuantDesk supplies the eyes, tools and execution, and keeps the final
//! safety boundary deterministic. A trade request becomes a proposal that
//! the Decision Engine and Risk Gate judge like any other. The agent's
//! allocation can only shrink there, never grow. Halts, loss limits and the
//! INV-14 live gates apply unchanged, and the Order Gateway is the only
//! order path. Paper is the default book.
//!
//! - [`llm`]: tool calling and web research on OpenAI, xAI and Gemini.
//! - [`tools`]: the tools the model calls.
//! - [`agent`]: the run loop, budgets, trace and prediction scoring.
//! - [`predictions`]: scoring rules and the scorecard.
//! - [`config`]: settings, the agent's identity, and the live gate.

pub mod agent;
pub mod config;
pub mod llm;
pub mod predictions;
pub mod tools;
