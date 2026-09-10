use crate::{
    entity::tracker,
    state::{Audience, Event, format_time},
};
fn location(value: Option<&str>) -> String {
    value.map(|s| format!("\n{s}")).unwrap_or_default()
}
pub(crate) fn started(event: &Event) -> anyhow::Result<String> {
    Ok(format!(
        "InReach hike started at {}.{}",
        format_time(event.event_at),
        location(event.location.as_deref())
    ))
}
pub(crate) fn recovery(event: &Event) -> anyhow::Result<String> {
    Ok(format!(
        "InReach contact resumed at {}.{}",
        format_time(event.event_at),
        location(event.location.as_deref())
    ))
}
pub(crate) fn finished(event: &Event) -> anyhow::Result<String> {
    Ok(format!(
        "InReach hike FINISHED at {}.{}\n\n{}",
        format_time(event.event_at),
        location(event.location.as_deref()),
        event.body
    ))
}
pub(crate) fn unrecognized(event: &Event) -> anyhow::Result<String> {
    Ok(format!(
        "SAFETY ALERT: unrecognized InReach message\nEvent time: {}\n\n{}",
        format_time(event.event_at),
        event.body
    ))
}
pub(crate) fn reminder(hike: &tracker::Model, audience: Audience, minutes: i64) -> String {
    let prefix = match audience {
        Audience::Owner => "No InReach OK",
        Audience::Safety => "SAFETY ALERT: no InReach OK",
    };
    format!(
        "{prefix} for {minutes} minutes. Last contact: {}.{}\n\n{}",
        format_time(hike.last_ok_at.expect("active hike has last OK")),
        location(hike.location.as_deref()),
        hike.last_body.as_deref().unwrap_or_default()
    )
}
