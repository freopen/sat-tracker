use chrono::{DateTime, Duration, Timelike, Utc};
use sea_orm::{DeriveActiveEnum, EnumIter};

pub(crate) const OWNER_MINUTES: &[i64] = &[30];
pub(crate) const SAFETY_MINUTES: &[i64] = &[60];
pub(crate) const FINISHED_COOLDOWN: Duration = Duration::minutes(5);

/// The UTC timestamp used by persisted state and business logic.
pub type DateTimeUtc = DateTime<Utc>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum Phase {
    #[sea_orm(string_value = "idle")]
    Idle,
    #[sea_orm(string_value = "active")]
    Active,
    #[sea_orm(string_value = "finished")]
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum IngressSource {
    #[sea_orm(string_value = "mail")]
    Mail,
    #[sea_orm(string_value = "telegram")]
    Telegram,
}

impl IngressSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Telegram => "telegram",
        }
    }
}

pub(crate) struct RawMail {
    pub bytes: Vec<u8>,
    pub received_at: DateTimeUtc,
}
pub(crate) struct Event {
    pub event_at: DateTimeUtc,
    pub body: String,
    pub location: Option<String>,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Audience {
    Owner,
    Safety,
}

pub(crate) fn format_time(time: DateTimeUtc) -> String {
    time.to_rfc3339()
}

/// Keep the persisted clock precision stable while using chrono's rich type
/// for all comparisons and arithmetic.
pub(crate) fn normalize(time: DateTimeUtc) -> DateTimeUtc {
    time.with_nanosecond(time.timestamp_subsec_millis() * 1_000_000)
        .expect("millisecond precision is valid for every chrono timestamp")
}
