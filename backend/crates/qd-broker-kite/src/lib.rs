//! QuantDesk adapter for Zerodha Kite Connect v3 (ADR 0014,
//! `docs/integrations/kite.md`).
//!
//! - [`client`]: HTTP client, envelope and error classification.
//! - [`login`]: login URL, checksum and token exchange.
//! - [`market`]: instruments CSV and daily candles (completed bars only).
//! - [`broker`]: order executor (live orders only with the `live-orders`
//!   feature, INV-14), account reader, and fills for the daily cycle.
//! - [`runner`]: the live runner and automatic demotion on breach (INV-11).

pub mod broker;
pub mod client;
pub mod login;
pub mod market;
pub mod runner;
