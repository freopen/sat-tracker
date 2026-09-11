use crate::state::{DateTimeUtc, Phase};
use sea_orm::entity::prelude::*;

/// The singleton hike state. Phase is a Rust enum even though SQLite stores it
/// as text; lifecycle invariants remain enforced by migration CHECKs.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "tracker")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    pub phase: Phase,
    pub started_at: Option<DateTimeUtc>,
    pub started_location: Option<String>,
    pub last_event_at: Option<DateTimeUtc>,
    /// The timestamp from which the current reminder schedule is measured.
    /// An alert mail that starts a hike establishes this exactly like an OK
    /// mail, so the field remains present for every active hike.
    pub last_ok_at: Option<DateTimeUtc>,
    pub last_body: Option<String>,
    pub location: Option<String>,
    pub finished_at: Option<DateTimeUtc>,
    pub owner_reminders_sent: i64,
    pub safety_reminders_sent: i64,
    pub owner_alerted: bool,
    pub safety_alerted: bool,
}

impl ActiveModelBehavior for ActiveModel {}
