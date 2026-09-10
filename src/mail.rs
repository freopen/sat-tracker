use chrono::{DateTime, Utc};

use mail_parser::MessageParser;
use regex::Regex;
use tracing::error;

use crate::{
    config::Config,
    state::{Event, RawMail, normalize},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Signal {
    Ok,
    Finished,
    Alert,
}

pub(crate) struct ParsedMail {
    pub(crate) event: Event,
    pub(crate) signal: Signal,
}

pub(crate) fn parse(raw: RawMail, config: &Config) -> ParsedMail {
    let received_at = normalize(raw.received_at);
    let parsed = MessageParser::default().parse(&raw.bytes);
    let (body, header_date) = match parsed {
        Some(message) => {
            let body = message
                .body_text(0)
                .map(|value| value.into_owned())
                .unwrap_or_else(|| String::from_utf8_lossy(&raw.bytes).into_owned());
            (
                body,
                message
                    .date()
                    .and_then(|date| DateTime::<Utc>::from_timestamp(date.to_timestamp(), 0)),
            )
        }
        None => (String::from_utf8_lossy(&raw.bytes).into_owned(), None),
    };
    let event_at = match header_date {
        Some(date) => date.min(received_at),
        None => {
            error!("mail has a missing or invalid Date header; using receipt time");
            received_at
        }
    };
    let ok = config.ok_regex.is_match(&body);
    let finished = config.finished_regex.is_match(&body);
    let signal = match (ok, finished) {
        (true, false) => Signal::Ok,
        (false, true) => Signal::Finished,
        _ => Signal::Alert,
    };

    ParsedMail {
        event: Event {
            event_at,
            location: extract_location(&body),
            body,
        },
        signal,
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    fn config(_: String) -> Config {
        Config {
            ok_regex: Regex::new("ALL OK").unwrap(),
            finished_regex: Regex::new("FINISHED").unwrap(),
            owner_chat_id: 1,
            safety_chat_id: 2,
            telegram_api_url: String::new(),
            telegram_webhook_url: String::new(),
            telegram_bot_token: String::new(),
        }
    }
    #[test]
    fn parses_classifies_clamps_and_extracts_quoted_printable_mail() {
        let received_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let parsed = parse(
        RawMail {
            bytes: b"Date: Thu, 01 Jan 2099 00:00:00 +0000\nMessage-ID: <future>\n\nALL OK Lat 47.1 Lon 9.6 https://inreachlink.com/example.".to_vec(),
            received_at,
        },
        &config("http://localhost".into()),
    );
        assert_eq!(parsed.signal, Signal::Ok);
        assert_eq!(parsed.event.event_at, received_at);
        assert_eq!(
            parsed.event.location.as_deref(),
            Some("Lat 47.1 Lon 9.6\nhttps://inreachlink.com/example")
        );

        let ambiguous = parse(
            RawMail {
                bytes: b"\nALL OK and FINISHED".to_vec(),
                received_at,
            },
            &config("http://localhost".into()),
        );
        assert_eq!(ambiguous.signal, Signal::Alert);

        let synthetic = concat!(
            "From: Garmin InReach <noreply@example.test>\r\n",
            "To: tracker@example.test\r\n",
            "Date: Thu, 01 Jan 2026 12:00:00 +0000\r\n",
            "Message-ID: <synthetic-inreach@example.test>\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: text/plain; charset=\"UTF-8\"\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "\r\n",
            "ALL OK\r\n",
            "Lat 12.3456 Lon -65.4321\r\n",
            "https://inreachlink.com/synthetic-token?source=3Dtest\r\n",
        );
        let parsed = parse(
            RawMail {
                bytes: synthetic.as_bytes().to_vec(),
                received_at: DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap(),
            },
            &config("http://localhost".into()),
        );
        assert_eq!(parsed.signal, Signal::Ok);
        assert_eq!(
            parsed.event.location.as_deref(),
            Some(
                "Lat 12.3456 Lon -65.4321\n\
             https://inreachlink.com/synthetic-token?source=test"
            )
        );
    }

    #[test]
    fn event_time_uses_mail_date_when_valid_and_receipt_time_otherwise() {
        let received_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let dated = parse(
            RawMail {
                bytes: b"Date: Thu, 01 Jan 1970 00:00:00 +0000\r\n\r\nALL OK".to_vec(),
                received_at,
            },
            &config("http://localhost".into()),
        );
        assert_eq!(
            dated.event.event_at,
            DateTime::<Utc>::from_timestamp(0, 0).unwrap()
        );

        let invalid = parse(
            RawMail {
                bytes: b"Date: not a date\r\n\r\nALL OK".to_vec(),
                received_at,
            },
            &config("http://localhost".into()),
        );
        assert_eq!(invalid.event.event_at, received_at);
    }
}
