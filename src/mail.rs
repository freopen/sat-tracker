use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mail_parser::MessageParser;
use regex::Regex;
use tracing::error;

use crate::{
    config::Config,
    state::{Event, RawMail},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Classification {
    Ok,
    Finished,
    Unrecognized,
}

pub(crate) struct ParsedMail {
    pub(crate) event: Event,
    pub(crate) classification: Classification,
}

pub(crate) fn parse(raw: RawMail, config: &Config) -> ParsedMail {
    let parsed = MessageParser::default().parse(&raw.bytes);
    let (message_id, body, header_date) = match parsed {
        Some(message) => {
            let body = message
                .body_text(0)
                .map(|value| value.into_owned())
                .unwrap_or_else(|| String::from_utf8_lossy(&raw.bytes).into_owned());
            (
                message.message_id().map(str::to_owned),
                body,
                message
                    .date()
                    .and_then(|date| system_time(date.to_timestamp())),
            )
        }
        None => (None, String::from_utf8_lossy(&raw.bytes).into_owned(), None),
    };
    let event_at = match header_date {
        Some(date) => date.min(raw.received_at),
        None => {
            error!("mail has a missing or invalid Date header; using receipt time");
            raw.received_at
        }
    };
    let ok = config.ok_regex.is_match(&body);
    let finished = config.finished_regex.is_match(&body);
    let classification = match (ok, finished) {
        (true, false) => Classification::Ok,
        (false, true) => Classification::Finished,
        _ => Classification::Unrecognized,
    };

    ParsedMail {
        event: Event {
            message_id,
            event_at,
            location: extract_location(&body),
            body,
        },
        classification,
    }
}

fn system_time(timestamp: i64) -> Option<SystemTime> {
    if timestamp >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(timestamp as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(timestamp.unsigned_abs()))
    }
}

pub(crate) fn extract_location(body: &str) -> Option<String> {
    let url = Regex::new(r"https?://inreachlink\.com/[^\s]+")
        .expect("location URL regex is valid")
        .find(body)
        .map(|matched| {
            matched
                .as_str()
                .trim_end_matches(['.', ',', ')'])
                .to_owned()
        });
    let coordinates = Regex::new(r"Lat\s+-?\d+(?:\.\d+)?\s+Lon\s+-?\d+(?:\.\d+)?")
        .expect("coordinate regex is valid")
        .find(body)
        .map(|matched| matched.as_str().to_owned());
    match (url, coordinates) {
        (Some(url), Some(coordinates)) => Some(format!("{coordinates}\n{url}")),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}
