use crate::time::{DateTimeUtc, now};
use crate::{app::Ctx, db::tracker};
use chrono::Duration;
use minijinja::{Environment, Error, ErrorKind, UndefinedBehavior, Value};
use regex::Regex;
use sea_orm::TryIntoModel;
use serde::{Deserialize, Serialize};
use tracing::warn;

pub(crate) const DEFAULT_SAFETY_ALERT_TEMPLATE: &str = r#"{% if last_alert is not none %}**SAFETY ALERT:** {{ last_alert }}{% else %}**SAFETY ALERT:** No OK has been received since {{ last_ok_at }}.{% endif %}

*Here are details to help locate the hiker.*

- **Hike started:** {{ started_at | date }} at {{ started_at | time }}
- **Starting location:** {% if started_location is not none %}{{ started_location }}{% else %}not recorded{% endif %}
- **Most recent event:** {{ last_event_at }}
- **Last OK:** {{ last_ok_at }}
- **Last known location:** {% if location is not none %}{{ location }}{% else %}not recorded{% endif %}

*Contact the hiker and use the locations above to decide what to do next.*"#;

pub(crate) const DEFAULT_SAFETY_RECOVERY_TEMPLATE: &str = r#"{% if active %}**SAFETY CONTACT RESUMED:** The hike continues normally.{% else %}**SAFETY CONTACT RESUMED:** The hike was finished without issues.{% endif %}"#;

pub(crate) const MAX_MESSAGE_CHARS: usize = 32_768;
pub(crate) const MAX_TEMPLATE_BYTES: usize = MAX_MESSAGE_CHARS * 4;
const DATETIME_VALUE_FIELD: &str = "__sat_tracker_datetime";
const DATETIME_FORMAT_FIELD: &str = "__sat_tracker_datetime_format";
const GUIDE_SOURCE: &str = include_str!("guide.md");
const GUIDE_TEMPLATE_NAME: &str = "guide";

#[derive(Serialize)]
struct GuideContext {
    alert: tracker::Model,
    recovery: tracker::Model,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TemplateId {
    SafetyAlert,
    SafetyRecovery,
}

impl TemplateId {
    pub(crate) fn set(self, ctx: &mut Ctx, source: &str) -> anyhow::Result<()> {
        self.set_in_environment(&mut ctx.templates, source)?;
        match self {
            Self::SafetyAlert => ctx.settings.safety_alert_template.set_ne(source.to_owned()),
            Self::SafetyRecovery => ctx
                .settings
                .safety_recovery_template
                .set_ne(source.to_owned()),
        }
        Ok(())
    }

    pub(crate) fn render(self, ctx: &Ctx) -> anyhow::Result<String> {
        let tracker = ctx.tracker.clone().try_into_model()?;
        render_named(&ctx.templates, self.name(), &tracker)
    }

    pub(crate) fn render_examples(self, ctx: &Ctx) -> anyhow::Result<Vec<String>> {
        self.example_trackers()
            .iter()
            .map(|tracker| render_named(&ctx.templates, self.name(), tracker))
            .collect()
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::SafetyAlert => "safety_alert",
            Self::SafetyRecovery => "safety_recovery",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SafetyAlert => "Safety alert template",
            Self::SafetyRecovery => "Safety recovery template",
        }
    }

    pub(crate) fn source(self, ctx: &Ctx) -> &str {
        match self {
            Self::SafetyAlert => ctx.settings.safety_alert_template.as_ref(),
            Self::SafetyRecovery => ctx.settings.safety_recovery_template.as_ref(),
        }
    }

    fn fallback(self) -> &'static str {
        match self {
            Self::SafetyAlert => DEFAULT_SAFETY_ALERT_TEMPLATE,
            Self::SafetyRecovery => DEFAULT_SAFETY_RECOVERY_TEMPLATE,
        }
    }

    fn example_trackers(self) -> Vec<tracker::Model> {
        match self {
            Self::SafetyAlert => vec![example_tracker(true), example_tracker(false)],
            Self::SafetyRecovery => vec![example_recovery_tracker()],
        }
    }

    fn set_in_environment(
        self,
        environment: &mut Environment<'static>,
        source: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            source.len() <= MAX_TEMPLATE_BYTES,
            "template is too large (maximum {MAX_TEMPLATE_BYTES} bytes)"
        );
        for example in self.example_trackers() {
            render_source(environment, source, &example)?;
        }
        environment
            .add_template_owned(self.name().to_owned(), source.to_owned())
            .map_err(|error| anyhow::anyhow!("template compilation failed: {}", error.kind()))
    }
}

pub(crate) fn new_environment(
    alert_source: &str,
    recovery_source: &str,
) -> anyhow::Result<Environment<'static>> {
    let mut environment = Environment::new();
    environment.set_debug(false);
    environment.remove_global("debug");
    environment.remove_filter("pprint");
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    environment.set_fuel(Some(100_000));
    environment.set_formatter(markdown_formatter);
    environment.add_filter("date", date_filter);
    environment.add_filter("time", time_filter);
    environment
        .add_template_owned(GUIDE_TEMPLATE_NAME.to_owned(), GUIDE_SOURCE.to_owned())
        .map_err(|error| anyhow::anyhow!("guide compilation failed: {}", error.kind()))?;
    install_template(&mut environment, TemplateId::SafetyAlert, alert_source)?;
    install_template(
        &mut environment,
        TemplateId::SafetyRecovery,
        recovery_source,
    )?;
    Ok(environment)
}

pub(crate) fn render_guide(environment: &Environment<'static>) -> anyhow::Result<String> {
    render_named(
        environment,
        GUIDE_TEMPLATE_NAME,
        &GuideContext {
            alert: example_tracker(true),
            recovery: example_recovery_tracker(),
        },
    )
}

fn install_template(
    environment: &mut Environment<'static>,
    template: TemplateId,
    source: &str,
) -> anyhow::Result<()> {
    match template.set_in_environment(environment, source) {
        Ok(()) => Ok(()),
        Err(error) => {
            warn!(
                template = template.name(),
                reason = "configured_template_invalid",
                kind = %error,
                "using built-in template fallback"
            );
            template.set_in_environment(environment, template.fallback())
        }
    }
}

fn render_named<S: serde::Serialize>(
    environment: &Environment<'static>,
    name: &str,
    value: &S,
) -> anyhow::Result<String> {
    let text = environment
        .get_template(name)
        .map_err(|error| anyhow::anyhow!("template lookup failed: {}", error.kind()))?
        .render(value)
        .map_err(|error| anyhow::anyhow!("template rendering failed: {}", error.kind()))?;
    validate_rendered(text)
}

fn render_source<S: serde::Serialize>(
    environment: &Environment<'static>,
    source: &str,
    value: &S,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        source.len() <= MAX_TEMPLATE_BYTES,
        "template is too large (maximum {MAX_TEMPLATE_BYTES} bytes)"
    );
    let text = environment.render_str(source, value).map_err(|error| {
        let stage = if error.kind() == ErrorKind::SyntaxError {
            "compilation"
        } else {
            "rendering"
        };
        anyhow::anyhow!("template {stage} failed: {}", error.kind())
    })?;
    validate_rendered(text)
}

fn validate_rendered(text: String) -> anyhow::Result<String> {
    let text = text.trim_end_matches(['\r', '\n']).to_owned();
    anyhow::ensure!(
        !text.trim().is_empty(),
        "template rendered an empty message"
    );
    anyhow::ensure!(
        text.chars().count() <= MAX_MESSAGE_CHARS,
        "template rendered more than {MAX_MESSAGE_CHARS} characters"
    );
    Ok(text)
}

fn example_tracker(with_alert: bool) -> tracker::Model {
    let now = now();
    tracker::Model {
        id: 1,
        active: true,
        started_at: Some(now - Duration::hours(3) - Duration::minutes(15)),
        started_location: Some(tracker::Location(-27.1500, -109.4333)),
        last_event_at: Some(now - Duration::minutes(3)),
        last_ok_at: Some(now - Duration::hours(1)),
        last_alert: with_alert
            .then_some("Route appears to have become unusually festive. 🗿🎄".to_owned()),
        location: Some(tracker::Location(-10.4217, 105.6791)),
        finished_at: DateTimeUtc::from_timestamp_millis(0).unwrap(),
        owner_reminders_sent: 0,
        safety_reminders_sent: 0,
        safety_alerted: with_alert,
    }
}

fn example_recovery_tracker() -> tracker::Model {
    let mut tracker = example_tracker(false);
    tracker.active = false;
    tracker.finished_at = tracker.last_event_at.expect("example has a last event");
    tracker.safety_alerted = true;
    tracker
}

fn markdown_formatter(
    output: &mut minijinja::Output<'_>,
    _state: &minijinja::State,
    value: &Value,
) -> Result<(), Error> {
    if value.is_undefined() {
        return Err(Error::new(
            ErrorKind::UndefinedError,
            "template referenced an undefined value",
        ));
    }
    if value.is_none() {
        return Ok(());
    }
    if let Some((latitude, longitude)) = telegram_location(value) {
        return output
            .write_str(&format!(
                r#"<tg-map lat="{latitude}" long="{longitude}" zoom="16"/>"#
            ))
            .map_err(|_| Error::from(ErrorKind::WriteFailure));
    }
    if let Some((timestamp, format)) = telegram_datetime(value) {
        let seconds = timestamp.timestamp();
        let fallback = markdown_escape(&timestamp.to_rfc3339());
        return output
            .write_str(&format!(
                "![{fallback}](tg://time?unix={seconds}&format={format})"
            ))
            .map_err(|_| Error::from(ErrorKind::WriteFailure));
    }
    let text = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    let text = if value.is_safe() {
        text
    } else {
        markdown_escape(&text)
    };
    output
        .write_str(&text)
        .map_err(|_| Error::from(ErrorKind::WriteFailure))
}

fn telegram_location(value: &Value) -> Option<(f64, f64)> {
    if value.len()? != 2 {
        return None;
    }
    let latitude: f64 = value.get_item_by_index(0).ok()?.try_into().ok()?;
    let longitude: f64 = value.get_item_by_index(1).ok()?.try_into().ok()?;
    if !latitude.is_finite()
        || !longitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
    {
        return None;
    }
    Some((latitude, longitude))
}

fn markdown_escape(value: &str) -> String {
    static ESCAPE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"[\\&<>_*\[\]()~`#+=\-|{}.!]").expect("markdown escape regex is valid")
    });
    ESCAPE
        .replace_all(value, |matched: &regex::Captures<'_>| {
            format!("\\{}", matched.get(0).expect("regex match exists").as_str())
        })
        .into_owned()
}

fn date_filter(value: Value) -> Result<Value, Error> {
    datetime_filter(value, "d")
}

fn time_filter(value: Value) -> Result<Value, Error> {
    datetime_filter(value, "t")
}

fn datetime_filter(value: Value, format: &str) -> Result<Value, Error> {
    if value.is_undefined() {
        return Err(Error::new(
            ErrorKind::UndefinedError,
            "datetime filter received an undefined value",
        ));
    }
    if value.is_none() {
        return Ok(Value::from_safe_string(String::new()));
    }
    telegram_datetime(&value)
        .map(|(timestamp, _)| timestamp)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                "datetime filter expects an RFC 3339 timestamp",
            )
        })?;
    Ok(Value::from_iter([
        (DATETIME_VALUE_FIELD, value),
        (DATETIME_FORMAT_FIELD, format.into()),
    ]))
}

fn telegram_datetime(value: &Value) -> Option<(DateTimeUtc, String)> {
    if let Some(format) = value
        .get_attr(DATETIME_FORMAT_FIELD)
        .ok()
        .and_then(|format| format.as_str().map(str::to_owned))
    {
        let value = value.get_attr(DATETIME_VALUE_FIELD).ok()?;
        return parse_datetime(&value).map(|timestamp| (timestamp, format));
    }
    parse_datetime(value).map(|timestamp| (timestamp, "r".to_owned()))
}

fn parse_datetime(value: &Value) -> Option<DateTimeUtc> {
    if let Some(value) = value
        .get_attr(DATETIME_VALUE_FIELD)
        .ok()
        .filter(|value| !value.is_undefined())
    {
        return parse_datetime(&value);
    }
    DateTimeUtc::deserialize(value.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_values_render_with_default_and_selected_formats() {
        let tracker = example_tracker(false);
        let environment = new_environment("{{ last_ok_at }}", "{{ last_ok_at }}")
            .expect("test template environment is valid");
        let last_ok_at = tracker.last_ok_at.expect("example has a last OK");
        let expected = |format| {
            format!(
                "![{}](tg://time?unix={}&format={format})",
                markdown_escape(&last_ok_at.to_rfc3339()),
                last_ok_at.timestamp()
            )
        };

        assert_eq!(
            environment
                .render_str("{{ last_ok_at }}", &tracker)
                .expect("bare timestamp renders"),
            expected("r")
        );
        assert_eq!(
            environment
                .render_str("{{ last_ok_at | date }}", &tracker)
                .expect("date filter renders"),
            expected("d")
        );
        assert_eq!(
            environment
                .render_str("{{ last_ok_at | time }}", &tracker)
                .expect("time filter renders"),
            expected("t")
        );
    }

    #[test]
    fn location_values_render_as_telegram_map_components() {
        let mut tracker = example_tracker(false);
        let environment = new_environment("{{ location }}", "{{ location }}")
            .expect("test template environment is valid");

        assert_eq!(
            environment
                .render_str("{{ location }}", &tracker)
                .expect("comma-separated location renders"),
            r#"<tg-map lat="-10.4217" long="105.6791" zoom="16"/>"#
        );

        tracker.location = Some(tracker::Location(47.3769, 8.5417));
        assert_eq!(
            environment
                .render_str("{{ location }}", &tracker)
                .expect("JSON location renders"),
            r#"<tg-map lat="47.3769" long="8.5417" zoom="16"/>"#
        );
    }

    #[test]
    fn inline_control_lines_have_explicit_whitespace() {
        let environment = new_environment("unused", "unused").expect("valid environment");
        assert!(!environment.trim_blocks());
        assert!(!environment.lstrip_blocks());
        let source = "Title\n\n{% if last_alert is not none %}Message: {{ last_alert }}{% endif %}\n{% if location is not none %}Location: {{ location }}{% endif %}\nAfter";
        let with_optional = example_tracker(true);
        let rendered = environment
            .render_str(source, &with_optional)
            .expect("template renders");
        assert_eq!(
            rendered,
            "Title\n\nMessage: Route appears to have become unusually festive\\. 🗿🎄\nLocation: <tg-map lat=\"-10.4217\" long=\"105.6791\" zoom=\"16\"/>\nAfter"
        );
    }

    #[test]
    fn guide_contains_rendered_time_and_location_widgets() {
        let ctx = crate::app::make_test_ctx();
        let guide = render_guide(&ctx.templates).expect("guide renders");
        assert!(guide.contains("tg://time?unix="));
        let map = guide
            .find(r#"<tg-map lat="-10.4217" long="105.6791" zoom="16"/>"#)
            .expect("guide has a rendered map");
        let first_code_block = guide.find("```").expect("guide has source examples");
        assert!(first_code_block < map);
        assert!(guide.contains("{{ started_at | date }}"));
        assert!(guide.contains("{{ last_event_at }}"));
        assert!(guide.contains("{{ last_ok_at }}"));
    }

    #[test]
    fn template_candidates_are_validated_before_they_are_saved() {
        let mut ctx = crate::app::make_test_ctx();
        let original_source = TemplateId::SafetyAlert.source(&ctx).to_owned();
        let original_render = TemplateId::SafetyAlert
            .render(&ctx)
            .expect("default template renders");

        assert!(
            TemplateId::SafetyAlert
                .set(&mut ctx, "{{ missing }}")
                .is_err()
        );
        assert_eq!(TemplateId::SafetyAlert.source(&ctx), original_source);
        assert_eq!(
            TemplateId::SafetyAlert
                .render(&ctx)
                .expect("unchanged template renders"),
            original_render
        );

        TemplateId::SafetyAlert
            .set(&mut ctx, "**{{ active }}**")
            .expect("candidate renders examples");
        let examples = TemplateId::SafetyAlert
            .render_examples(&ctx)
            .expect("saved template renders examples");
        assert_eq!(examples.len(), 2);
        assert!(examples[0].contains("True"));
    }
}
