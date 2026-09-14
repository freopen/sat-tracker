use crate::time::normalize;
use crate::{app::Ctx, db::tracker::Location, hike, notify};
use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use mail_parser::MessageParser;
use regex::Regex;
use std::sync::LazyLock;

const FINISHED_COOLDOWN: Duration = Duration::minutes(5);

pub(crate) async fn process(ctx: &mut Ctx) -> anyhow::Result<()> {
    let (payload, received_at) = {
        let row = ctx.inbox.as_ref().context("pending inbox row missing")?;
        (
            row.payload
                .as_deref()
                .context("pending inbox payload missing")?,
            row.received_at,
        )
    };
    let received_at = normalize(received_at);
    let parsed = MessageParser::default().parse(payload);
    let (body, header_date) = match parsed {
        Some(message) => {
            let body = message
                .body_text(0)
                .map(|value| value.into_owned())
                .unwrap_or_else(|| String::from_utf8_lossy(payload).into_owned());
            let date = message
                .date()
                .and_then(|date| DateTime::<Utc>::from_timestamp(date.to_timestamp(), 0));
            (body, date)
        }
        None => (String::from_utf8_lossy(payload).into_owned(), None),
    };
    let at = match header_date {
        Some(date) => normalize(date).min(received_at),
        None => {
            tracing::warn!(
                reason = "missing_or_invalid_mail_date",
                "using mail receipt time"
            );
            received_at
        }
    };
    let location = extract_location(&body);
    let last_event_at = *ctx.tracker.last_event_at.as_ref();
    if last_event_at.is_none_or(|last_event_at| last_event_at < at) {
        ctx.tracker.last_event_at.set_ne(Some(at));
        ctx.tracker.location = sea_orm::Set(location);
    }
    let active = *ctx.tracker.active.as_ref();

    match (
        ctx.config.ok_regex.is_match(&body),
        ctx.config.finished_regex.is_match(&body),
    ) {
        (true, false) => {
            if !active
                && at
                    <= (*ctx.tracker.finished_at.as_ref())
                        .checked_add_signed(FINISHED_COOLDOWN)
                        .context("finished-hike cooldown overflow")?
            {
                tracing::info!(
                    reason = "finished_cooldown",
                    "mail OK ignored during finished-hike cooldown"
                );
                return Ok(());
            }
            hike::receive_ok(ctx).await?;
        }
        (false, true) => hike::finish_hike(ctx).await?,
        _ => {
            ctx.tracker.last_alert = sea_orm::Set(Some(body));
            if !active {
                hike::start_hike(ctx).await?;
                tracing::info!("started a new hike from alert mail");
            }
            notify::send_explicit_alert(ctx).await?;
        }
    }
    Ok(())
}

fn extract_location(body: &str) -> Option<Location> {
    static COORDINATES: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"Lat\s+(-?\d+(?:\.\d+)?)\s+Lon\s+(-?\d+(?:\.\d+)?)")
            .expect("coordinate regex is valid")
    });
    let captures = COORDINATES.captures(body)?;
    let latitude = captures.get(1)?.as_str().parse::<f64>().ok()?;
    let longitude = captures.get(2)?.as_str().parse::<f64>().ok()?;
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return None;
    }
    Some(Location(latitude, longitude))
}
