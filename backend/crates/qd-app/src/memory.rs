//! In-memory journal and halt store.
//!
//! These are real implementations, not fakes: backtests keep their journal and
//! halts in memory by design (no broker, no persistence needed), and tests use
//! them too. The server uses the PostgreSQL implementations; production
//! configuration cannot select these (ADR 0006).

use std::sync::Mutex;

use async_trait::async_trait;
use qd_domain::halt::Halt;

use crate::journal::JournalEntry;
use crate::ports::{HaltStore, Journal, JournalError, StoreError};

/// An append-only journal held in memory.
#[derive(Debug, Default)]
pub struct InMemoryJournal {
    entries: Mutex<Vec<JournalEntry>>,
}

impl InMemoryJournal {
    /// Creates an empty journal.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of every entry, in order.
    #[must_use]
    pub fn entries(&self) -> Vec<JournalEntry> {
        self.entries.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl Journal for InMemoryJournal {
    async fn append(&self, entry: &JournalEntry) -> Result<u64, JournalError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| JournalError("journal lock poisoned".to_owned()))?;
        entries.push(entry.clone());
        u64::try_from(entries.len()).map_err(|e| JournalError(e.to_string()))
    }
}

/// Halts held in memory. A cleared halt replaces the earlier version of itself.
#[derive(Debug, Default)]
pub struct InMemoryHaltStore {
    halts: Mutex<Vec<Halt>>,
}

impl InMemoryHaltStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl HaltStore for InMemoryHaltStore {
    async fn load(&self) -> Result<Vec<Halt>, StoreError> {
        self.halts
            .lock()
            .map(|h| h.clone())
            .map_err(|_| StoreError("halt store lock poisoned".to_owned()))
    }

    async fn record(&self, halt: &Halt) -> Result<(), StoreError> {
        let mut halts = self
            .halts
            .lock()
            .map_err(|_| StoreError("halt store lock poisoned".to_owned()))?;
        halts.retain(|h| h.id() != halt.id());
        halts.push(halt.clone());
        Ok(())
    }
}
