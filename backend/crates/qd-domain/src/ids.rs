//! Typed identifiers backed by UUIDv7 (spec §6.1).
//!
//! Each aggregate gets its own newtype so identifiers cannot be mixed up. IDs are
//! generated in the application; the creation time is passed in because domain
//! code never reads the clock.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::{NoContext, Timestamp, Uuid};

fn uuid_v7_at(at: DateTime<Utc>) -> Uuid {
    // UUIDv7 cannot encode instants before the Unix epoch, so they are clamped.
    // The time component only orders identifiers; it is never read back as data.
    let seconds = u64::try_from(at.timestamp()).unwrap_or(0);
    // chrono reports leap seconds as nanos >= 1e9; keep within one second.
    let nanos = at.timestamp_subsec_nanos().min(999_999_999);
    Uuid::new_v7(Timestamp::from_unix(NoContext, seconds, nanos))
}

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Creates a new UUIDv7 identifier whose time component is `at`.
            #[must_use]
            pub fn new_at(at: DateTime<Utc>) -> Self {
                Self(uuid_v7_at(at))
            }

            /// Wraps an existing UUID, for example one loaded from storage.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

define_id!(
    /// A trading account (paper or live).
    AccountId
);
define_id!(
    /// An instrument. Specs for one instrument are versioned under the same id.
    InstrumentId
);
define_id!(
    /// A strategy family.
    StrategyId
);
define_id!(
    /// One immutable version of a strategy (INV-10).
    StrategyVersionId
);
define_id!(
    /// A trade proposal.
    ProposalId
);
define_id!(
    /// A journaled decision.
    DecisionId
);
define_id!(
    /// An order intent handled by the Order Gateway.
    OrderIntentId
);
define_id!(
    /// A position managed by the Position Manager.
    PositionId
);
define_id!(
    /// A halt (kill-switch entry).
    HaltId
);
define_id!(
    /// An immutable point-in-time data snapshot (INV-09).
    SnapshotId
);
define_id!(
    /// A human user (the owner or a read-only user).
    UserId
);
define_id!(
    /// A recorded body of validation evidence backing a promotion (INV-11).
    EvidenceId
);
define_id!(
    /// A stored AI review. AI output is advisory only (INV-04).
    AiReviewId
);
define_id!(
    /// One run of the AI agent (ADR 0016).
    AgentRunId
);
define_id!(
    /// One AI prediction, tracked against the realized outcome (ADR 0016).
    PredictionId
);

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn ids_are_version_7_and_ordered_by_time() {
        let earlier = Utc.with_ymd_and_hms(2026, 1, 5, 9, 15, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 1, 5, 9, 16, 0).unwrap();
        let a = ProposalId::new_at(earlier);
        let b = ProposalId::new_at(later);
        assert_eq!(a.as_uuid().get_version_num(), 7);
        assert!(a < b);
    }

    #[test]
    fn ids_round_trip_through_strings_and_json() {
        let at = Utc.with_ymd_and_hms(2026, 1, 5, 9, 15, 0).unwrap();
        let id = HaltId::new_at(at);
        let parsed: HaltId = id.to_string().parse().unwrap();
        assert_eq!(parsed, id);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<HaltId>(&json).unwrap(), id);
    }

    #[test]
    fn pre_epoch_times_do_not_fail() {
        let at = Utc.with_ymd_and_hms(1960, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(UserId::new_at(at).as_uuid().get_version_num(), 7);
    }
}
