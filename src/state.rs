use std::{
    collections::VecDeque,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use durable_actions::ActionId;
use serde::{Deserialize, Serialize};

pub(crate) const OWNER_AFTER: Duration = Duration::from_secs(30 * 60);
pub(crate) const SAFETY_AFTER: Duration = Duration::from_secs(60 * 60);
pub(crate) const FINISHED_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const MAX_IDS: usize = 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct TrackerState {
    pub(crate) hike: HikeState,
    #[serde(default)]
    pub(crate) processed_ids: VecDeque<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) enum HikeState {
    #[default]
    Idle,
    Active(ActiveHike),
    Finished(FinishedHike),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ActiveHike {
    pub(crate) started_at: SystemTime,
    pub(crate) started_location: Option<String>,
    pub(crate) last_event_at: SystemTime,
    pub(crate) last_ok_at: SystemTime,
    pub(crate) last_body: String,
    pub(crate) location: Option<String>,
    pub(crate) owner_started_notified: bool,
    pub(crate) owner_alerted: bool,
    pub(crate) safety_alerted: bool,
    pub(crate) owner_alert_action: Option<ActionId>,
    pub(crate) safety_alert_action: Option<ActionId>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FinishedHike {
    pub(crate) event_at: SystemTime,
    pub(crate) body: String,
    pub(crate) location: Option<String>,
    pub(crate) owner_notified: bool,
    pub(crate) safety_notified: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RawMail {
    pub(crate) bytes: Vec<u8>,
    pub(crate) received_at: SystemTime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Event {
    pub(crate) message_id: Option<String>,
    pub(crate) event_at: SystemTime,
    pub(crate) body: String,
    pub(crate) location: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct AlertParameters {
    pub(crate) expected_last_ok_at: SystemTime,
    pub(crate) audience: Audience,
    pub(crate) payload: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum AlertSignal {
    Unrecognized { event: Event },
    Overdue(AlertParameters),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum Audience {
    Owner,
    Safety,
}

pub(crate) fn push_bounded(values: &mut VecDeque<String>, value: String) {
    values.push_back(value);
    while values.len() > MAX_IDS {
        values.pop_front();
    }
}

pub(crate) fn location_suffix(location: Option<&str>) -> String {
    location
        .map(|value| format!("\n{value}"))
        .unwrap_or_default()
}

pub(crate) fn format_time(time: SystemTime) -> String {
    mail_parser::DateTime::from_timestamp(unix_timestamp(time)).to_rfc3339()
}

fn unix_timestamp(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_secs()).unwrap_or(i64::MAX),
    }
}
