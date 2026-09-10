use chrono::{DateTime, Duration, Timelike, Utc};
use sea_orm::{DeriveActiveEnum, EnumIter, FromJsonQueryResult};
use serde::{Deserialize, Serialize};

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
pub enum SettingsPosition {
    #[sea_orm(string_value = "main")]
    Main,
    #[sea_orm(string_value = "settings")]
    Settings,
    #[sea_orm(string_value = "owner_reminder_times")]
    OwnerReminderTimes,
    #[sea_orm(string_value = "safety_reminder_times")]
    SafetyReminderTimes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, FromJsonQueryResult)]
pub struct ReminderMinutes(pub Vec<i64>);

impl ReminderMinutes {
    pub(crate) fn parse(input: &str) -> anyhow::Result<Self> {
        let mut minutes = Vec::new();
        for part in input.split(',') {
            let part = part.trim();
            anyhow::ensure!(
                !part.is_empty(),
                "reminder times must not contain empty values"
            );
            let minute = part
                .parse::<i64>()
                .map_err(|_| anyhow::anyhow!("reminder times must be integers"))?;
            anyhow::ensure!(minute > 0, "reminder times must be positive");
            if let Some(previous) = minutes.last() {
                anyhow::ensure!(
                    minute > *previous,
                    "reminder times must be strictly increasing"
                );
            }
            minutes.push(minute);
        }
        anyhow::ensure!(!minutes.is_empty(), "reminder times must not be empty");
        Ok(Self(minutes))
    }

    pub(crate) fn as_slice(&self) -> &[i64] {
        &self.0
    }
}

impl std::fmt::Display for ReminderMinutes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = self
            .0
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        formatter.write_str(&value)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminder_minutes_parse_and_format_canonical_values() {
        let minutes = ReminderMinutes::parse(" 30, 45, 60 ").unwrap();
        assert_eq!(minutes.as_slice(), &[30, 45, 60]);
        assert_eq!(minutes.to_string(), "30, 45, 60");
    }

    #[test]
    fn reminder_minutes_reject_invalid_values() {
        for input in [
            "", " ", ",", "30,", "30,,60", "0", "-1", "thirty", "45, 30", "30, 30",
        ] {
            assert!(
                ReminderMinutes::parse(input).is_err(),
                "accepted: {input:?}"
            );
        }
    }
}
