//! Every strategy implementation in this build, with its parameters as the
//! Strategy Registry stores them. A registered version runs only if its
//! logic version is here and its parameters are equal (INV-10).

use serde_json::Value;

use crate::strategy::Strategy;
use crate::trend_pullback::TrendPullback;

/// One implementation.
pub struct CatalogEntry {
    /// The logic.
    pub strategy: Box<dyn Strategy>,
    /// Its parameters, serialized as the registry stores them.
    pub parameters: Value,
}

impl std::fmt::Debug for CatalogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogEntry")
            .field("logic_version", &self.strategy.logic_version())
            .finish_non_exhaustive()
    }
}

/// The catalog. Fails only if parameters cannot be serialized.
pub fn catalog() -> Result<Vec<CatalogEntry>, serde_json::Error> {
    let trend = TrendPullback::v1();
    let parameters = serde_json::to_value(trend.params())?;
    Ok(vec![CatalogEntry {
        strategy: Box::new(trend),
        parameters,
    }])
}

/// The implementation for a registered version, if the build has it with
/// exactly these parameters.
#[must_use]
pub fn find<'a>(
    entries: &'a [CatalogEntry],
    logic_version: &str,
    parameters: &Value,
) -> Option<&'a CatalogEntry> {
    entries
        .iter()
        .find(|c| c.strategy.logic_version() == logic_version && &c.parameters == parameters)
}
