//! MiniJinja templates used for Telegram messages.
//!
//! Templates are deliberately rendered here, at the edge of the application.
//! The tracker and scheduler continue to deal in UTC `DateTime` values while
//! the template context exposes Unix seconds, which makes the `time` filter
//! useful for Telegram's reader-local date entities.

use crate::{
    entity::{settings, tracker},
    state::{DateTimeUtc, Event, IngressSource, Phase},
};
use anyhow::anyhow;
use minijinja::{
    Environment, Error, ErrorKind, UndefinedBehavior, Value,
    value::{Kwargs, Rest, from_args},
};
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use std::sync::{LazyLock, RwLock};

/// Telegram accepts up to 32,768 UTF-8 characters in a rich message.
pub(crate) const MAX_MESSAGE_CHARS: usize = 32_768;
/// Template sources may be up to four times the rich-message character limit;
/// source size is measured in UTF-8 bytes.
pub(crate) const MAX_TEMPLATE_BYTES: usize = MAX_MESSAGE_CHARS * 4;

/// This is intentionally a small, useful default.  The owner can replace it
/// from Telegram after seeing the rendered examples.
pub(crate) const DEFAULT_SAFETY_ALERT_TEMPLATE: &str = r#"**SAFETY ALERT: {{ alert.reason }}**
At: {{ alert.at | time("wDT") }}
{% if alert.threshold_minutes is not none %}No OK for {{ alert.elapsed_minutes }} minutes (threshold {{ alert.threshold_minutes }}).{% endif %}
{% if hike.location %}Location: {{ hike.location }}{% endif %}
{% if mail is not none and mail.location is not none %}Mail location: {{ mail.location }}{% endif %}
{% if mail is not none %}Message:
{{ mail.body }}{% endif %}"#;

pub(crate) const DEFAULT_SAFETY_RECOVERY_TEMPLATE: &str = r#"**SAFETY CONTACT RESUMED**
At: {{ alert.at | time("wDT") }}"#;

/// The owner guide is kept as a Markdown file and embedded at compile time so
/// deployments only need the final binary.
pub(crate) const GUIDE_SOURCE: &str = include_str!("guide.md");

pub(crate) const SETTING_PROMPT: &str = "{{ label }} are currently {{ value }}.\nType a new comma-separated list of positive, strictly increasing minutes (for example, 30, 45, 60).";
pub(crate) const SETTING_UPDATED: &str = "{{ label }} updated to {{ value }}.";
pub(crate) const SETTING_INVALID: &str = "Invalid reminder times.\n\n{{ label }} are currently {{ value }}.\nType a new comma-separated list of positive, strictly increasing minutes (for example, 30, 45, 60).";
pub(crate) const SAFETY_TEMPLATE_PROMPT: &str =
    "{{ label }}:\n\n{{ source }}\n\nSend a new template, or choose an action below.";
pub(crate) const SAFETY_TEMPLATE_INVALID: &str =
    "{{ label }} was not saved. {{ error }}\n\nSend another template, or choose an action below.";
pub(crate) const SAFETY_TEMPLATE_UPDATED: &str = "{{ label }} updated.";
pub(crate) const OWNER_REMINDER: &str = "No InReach OK for {{ minutes }} minutes.";

/// Every message source is named explicitly so call sites select a template
/// without passing arbitrary source text into the renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum TemplateId {
    OwnerMainMenu,
    OwnerVersion,
    OwnerSettingsMenu,
    OwnerSettingsUnavailable,
    OwnerStarted,
    OwnerOk,
    OwnerRecovery,
    OwnerFinished,
    OwnerFallback,
    OwnerReminder,
    SettingPrompt,
    SettingUpdated,
    SettingInvalid,
    SafetyTemplatePrompt,
    SafetyTemplateInvalid,
    SafetyTemplateUpdated,
    SafetyAlert,
    SafetyRecovery,
    Guide,
}

impl TemplateId {
    fn name(self) -> &'static str {
        match self {
            Self::OwnerMainMenu => "owner_main_menu",
            Self::OwnerVersion => "owner_version",
            Self::OwnerSettingsMenu => "owner_settings_menu",
            Self::OwnerSettingsUnavailable => "owner_settings_unavailable",
            Self::OwnerStarted => "owner_started",
            Self::OwnerOk => "owner_ok",
            Self::OwnerRecovery => "owner_recovery",
            Self::OwnerFinished => "owner_finished",
            Self::OwnerFallback => "owner_fallback",
            Self::OwnerReminder => "owner_reminder",
            Self::SettingPrompt => "setting_prompt",
            Self::SettingUpdated => "setting_updated",
            Self::SettingInvalid => "setting_invalid",
            Self::SafetyTemplatePrompt => "safety_template_prompt",
            Self::SafetyTemplateInvalid => "safety_template_invalid",
            Self::SafetyTemplateUpdated => "safety_template_updated",
            Self::SafetyAlert => "safety_alert",
            Self::SafetyRecovery => "safety_recovery",
            Self::Guide => "guide",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct AlertValues {
    reason: String,
    at: i64,
    threshold_minutes: Option<i64>,
    deadline_at: Option<i64>,
    elapsed_minutes: Option<i64>,
    minutes_since_last_ok: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
struct MailValues {
    event_at: i64,
    received_at: i64,
    body: String,
    location: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct HikeValues {
    phase: String,
    started_at: Option<i64>,
    started_location: Option<String>,
    last_ok_at: Option<i64>,
    last_event_at: Option<i64>,
    last_body: Option<String>,
    location: Option<String>,
    finished_at: Option<i64>,
    owner_reminders_sent: i64,
    safety_reminders_sent: i64,
    owner_alerted: bool,
    safety_alerted: bool,
}

#[derive(Clone, Debug, Serialize)]
struct SettingsValues {
    owner_reminder_minutes: Vec<i64>,
    safety_reminder_minutes: Vec<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct AlertContext {
    alert: AlertValues,
    mail: Option<MailValues>,
    hike: HikeValues,
    settings: SettingsValues,
}

impl AlertContext {
    pub(crate) fn new(
        hike: &tracker::Model,
        settings: &settings::Model,
        event: Option<&Event>,
        reason: &str,
        at: DateTimeUtc,
        threshold_minutes: Option<i64>,
    ) -> Self {
        let reminder_base = hike.last_ok_at;
        let elapsed_minutes =
            reminder_base.map(|base| at.signed_duration_since(base).num_minutes());
        let minutes_since_last_ok = hike
            .last_ok_at
            .map(|last_ok| at.signed_duration_since(last_ok).num_minutes());
        let deadline_at = threshold_minutes.and_then(|minutes| {
            reminder_base
                .and_then(|base| base.checked_add_signed(chrono::Duration::minutes(minutes)))
                .map(unix_seconds)
        });
        Self {
            alert: AlertValues {
                reason: reason.to_owned(),
                at: unix_seconds(at),
                threshold_minutes,
                deadline_at,
                elapsed_minutes,
                minutes_since_last_ok,
            },
            mail: event.map(|event| MailValues {
                event_at: unix_seconds(event.event_at),
                received_at: unix_seconds(event.received_at),
                body: event.body.clone(),
                location: event.location.clone(),
            }),
            hike: HikeValues {
                phase: phase_name(hike.phase).to_owned(),
                started_at: hike.started_at.map(unix_seconds),
                started_location: hike.started_location.clone(),
                last_ok_at: hike.last_ok_at.map(unix_seconds),
                last_event_at: hike.last_event_at.map(unix_seconds),
                last_body: hike.last_body.clone(),
                location: hike.location.clone(),
                finished_at: hike.finished_at.map(unix_seconds),
                owner_reminders_sent: hike.owner_reminders_sent,
                safety_reminders_sent: hike.safety_reminders_sent,
                owner_alerted: hike.owner_alerted,
                safety_alerted: hike.safety_alerted,
            },
            settings: SettingsValues {
                owner_reminder_minutes: settings.owner_reminder_minutes.0.clone(),
                safety_reminder_minutes: settings.safety_reminder_minutes.0.clone(),
            },
        }
    }
}

struct RendererState {
    environment: Environment<'static>,
    safety_alert_source: String,
    safety_recovery_source: String,
    safety_alert_error: bool,
    safety_recovery_error: bool,
}

/// A long-lived template registry owned by the Telegram service.  The
/// environment is built once at startup and all fixed sources are compiled
/// into it.  Configurable sources are replaced in the same environment after
/// a settings change.
pub(crate) struct Renderer {
    state: RwLock<RendererState>,
}

impl Renderer {
    pub(crate) fn new(
        safety_alert_source: &str,
        safety_recovery_source: &str,
    ) -> anyhow::Result<Self> {
        let mut environment = configured_environment();
        for (id, source) in hardcoded_templates() {
            environment
                .add_template_owned(id.name().to_owned(), source)
                .map_err(|error| anyhow!("template compilation failed: {}", error.kind()))?;
        }
        let safety_alert_error = install_configured_template(
            &mut environment,
            TemplateId::SafetyAlert,
            safety_alert_source,
            DEFAULT_SAFETY_ALERT_TEMPLATE,
        )?;
        let safety_recovery_error = install_configured_template(
            &mut environment,
            TemplateId::SafetyRecovery,
            safety_recovery_source,
            DEFAULT_SAFETY_RECOVERY_TEMPLATE,
        )?;
        Ok(Self {
            state: RwLock::new(RendererState {
                environment,
                safety_alert_source: safety_alert_source.to_owned(),
                safety_recovery_source: safety_recovery_source.to_owned(),
                safety_alert_error,
                safety_recovery_error,
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn defaults() -> anyhow::Result<Self> {
        Self::new(
            DEFAULT_SAFETY_ALERT_TEMPLATE,
            DEFAULT_SAFETY_RECOVERY_TEMPLATE,
        )
    }

    pub(crate) fn render<S: Serialize>(
        &self,
        id: TemplateId,
        context: &S,
    ) -> anyhow::Result<String> {
        let state = self
            .state
            .read()
            .map_err(|_| anyhow!("template environment lock poisoned"))?;
        if matches!(id, TemplateId::SafetyAlert) && state.safety_alert_error {
            return Err(anyhow!("configured safety alert template is unavailable"));
        }
        if matches!(id, TemplateId::SafetyRecovery) && state.safety_recovery_error {
            return Err(anyhow!(
                "configured safety recovery template is unavailable"
            ));
        }
        render_loaded(&state.environment, id, context)
    }

    pub(crate) fn render_alert(
        &self,
        hike: &tracker::Model,
        settings: &settings::Model,
        event: Option<&Event>,
        reason: &str,
        at: DateTimeUtc,
        threshold_minutes: Option<i64>,
    ) -> anyhow::Result<String> {
        let context = AlertContext::new(hike, settings, event, reason, at, threshold_minutes);
        self.render(TemplateId::SafetyAlert, &context)
    }

    pub(crate) fn render_recovery(
        &self,
        hike: &tracker::Model,
        settings: &settings::Model,
        event: &Event,
        at: DateTimeUtc,
    ) -> anyhow::Result<String> {
        let context = recovery_context(hike, settings, event, at);
        self.render(TemplateId::SafetyRecovery, &context)
    }

    pub(crate) fn example_alerts(&self) -> anyhow::Result<(String, String)> {
        let settings = example_settings();
        let hike = example_hike();
        let event = example_event();
        let mail = self.render_alert(
            &hike,
            &settings,
            Some(&event),
            "alert_mail",
            event.event_at,
            None,
        )?;
        let overdue =
            self.render_alert(&hike, &settings, None, "overdue", event.event_at, Some(60))?;
        Ok((mail, overdue))
    }

    pub(crate) fn example_recovery(&self) -> anyhow::Result<String> {
        let settings = example_settings();
        let hike = example_hike();
        let event = example_event();
        self.render_recovery(&hike, &settings, &event, event.event_at)
    }

    pub(crate) fn candidate_safety_alert(&self, source: &str) -> anyhow::Result<Self> {
        let state = self
            .state
            .read()
            .map_err(|_| anyhow!("template environment lock poisoned"))?;
        let mut environment = state.environment.clone();
        let safety_alert_error = install_configured_template(
            &mut environment,
            TemplateId::SafetyAlert,
            source,
            DEFAULT_SAFETY_ALERT_TEMPLATE,
        )?;
        Ok(Self {
            state: RwLock::new(RendererState {
                environment,
                safety_alert_source: source.to_owned(),
                safety_recovery_source: state.safety_recovery_source.clone(),
                safety_alert_error,
                safety_recovery_error: state.safety_recovery_error,
            }),
        })
    }

    pub(crate) fn candidate_safety_recovery(&self, source: &str) -> anyhow::Result<Self> {
        let state = self
            .state
            .read()
            .map_err(|_| anyhow!("template environment lock poisoned"))?;
        let mut environment = state.environment.clone();
        let safety_recovery_error = install_configured_template(
            &mut environment,
            TemplateId::SafetyRecovery,
            source,
            DEFAULT_SAFETY_RECOVERY_TEMPLATE,
        )?;
        Ok(Self {
            state: RwLock::new(RendererState {
                environment,
                safety_alert_source: state.safety_alert_source.clone(),
                safety_recovery_source: source.to_owned(),
                safety_alert_error: state.safety_alert_error,
                safety_recovery_error,
            }),
        })
    }

    pub(crate) fn guide(&self) -> anyhow::Result<String> {
        self.render(TemplateId::Guide, &json!({}))
    }

    pub(crate) fn sync_sources(
        &self,
        safety_alert_source: &str,
        safety_recovery_source: &str,
    ) -> anyhow::Result<()> {
        let mut state = self
            .state
            .write()
            .map_err(|_| anyhow!("template environment lock poisoned"))?;

        if state.safety_alert_source != safety_alert_source {
            state.safety_alert_error = install_configured_template(
                &mut state.environment,
                TemplateId::SafetyAlert,
                safety_alert_source,
                DEFAULT_SAFETY_ALERT_TEMPLATE,
            )?;
            state.safety_alert_source = safety_alert_source.to_owned();
        }
        if state.safety_recovery_source != safety_recovery_source {
            state.safety_recovery_error = install_configured_template(
                &mut state.environment,
                TemplateId::SafetyRecovery,
                safety_recovery_source,
                DEFAULT_SAFETY_RECOVERY_TEMPLATE,
            )?;
            state.safety_recovery_source = safety_recovery_source.to_owned();
        }
        Ok(())
    }

    pub(crate) fn set_safety_alert_source(&self, source: &str) -> anyhow::Result<()> {
        let recovery_source = self
            .state
            .read()
            .map_err(|_| anyhow!("template environment lock poisoned"))?
            .safety_recovery_source
            .clone();
        self.sync_sources(source, &recovery_source)
    }

    pub(crate) fn set_safety_recovery_source(&self, source: &str) -> anyhow::Result<()> {
        let alert_source = self
            .state
            .read()
            .map_err(|_| anyhow!("template environment lock poisoned"))?
            .safety_alert_source
            .clone();
        self.sync_sources(&alert_source, source)
    }
}

fn configured_environment<'source>() -> Environment<'source> {
    let mut environment = Environment::new();
    environment.set_debug(false);
    environment.remove_global("debug");
    environment.remove_filter("pprint");
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    environment.set_fuel(Some(100_000));
    environment.set_formatter(markdown_formatter);
    environment.add_filter("time", time_filter);
    environment
}

fn hardcoded_templates() -> Vec<(TemplateId, String)> {
    vec![
        (TemplateId::OwnerMainMenu, OWNER_MAIN_MENU.to_owned()),
        (TemplateId::OwnerVersion, OWNER_VERSION.to_owned()),
        (
            TemplateId::OwnerSettingsMenu,
            OWNER_SETTINGS_MENU.to_owned(),
        ),
        (
            TemplateId::OwnerSettingsUnavailable,
            OWNER_SETTINGS_UNAVAILABLE.to_owned(),
        ),
        (TemplateId::OwnerStarted, OWNER_STARTED.to_owned()),
        (TemplateId::OwnerOk, OWNER_OK.to_owned()),
        (TemplateId::OwnerRecovery, OWNER_RECOVERY.to_owned()),
        (TemplateId::OwnerFinished, OWNER_FINISHED.to_owned()),
        (TemplateId::OwnerFallback, OWNER_FALLBACK.to_owned()),
        (TemplateId::OwnerReminder, OWNER_REMINDER.to_owned()),
        (TemplateId::SettingPrompt, SETTING_PROMPT.to_owned()),
        (TemplateId::SettingUpdated, SETTING_UPDATED.to_owned()),
        (TemplateId::SettingInvalid, SETTING_INVALID.to_owned()),
        (
            TemplateId::SafetyTemplatePrompt,
            SAFETY_TEMPLATE_PROMPT.to_owned(),
        ),
        (
            TemplateId::SafetyTemplateInvalid,
            SAFETY_TEMPLATE_INVALID.to_owned(),
        ),
        (
            TemplateId::SafetyTemplateUpdated,
            SAFETY_TEMPLATE_UPDATED.to_owned(),
        ),
        (TemplateId::Guide, GUIDE_SOURCE.to_owned()),
    ]
    .into_iter()
    .collect()
}

fn add_configured_template(
    environment: &mut Environment<'static>,
    id: TemplateId,
    source: &str,
) -> anyhow::Result<()> {
    environment
        .add_template_owned(id.name().to_owned(), source.to_owned())
        .map_err(|error| anyhow!("template compilation failed: {}", error.kind()))
}

fn install_configured_template(
    environment: &mut Environment<'static>,
    id: TemplateId,
    source: &str,
    fallback: &str,
) -> anyhow::Result<bool> {
    if source.len() > MAX_TEMPLATE_BYTES {
        tracing::warn!(
            reason = "configured_template_too_large",
            template = id.name(),
            "configured Telegram template unavailable; using built-in fallback"
        );
        add_configured_template(environment, id, fallback)?;
        return Ok(true);
    }
    match add_configured_template(environment, id, source) {
        Ok(()) => Ok(false),
        Err(error) => {
            tracing::warn!(
                reason = "configured_template_compile_failed",
                template = id.name(),
                kind = %error,
                "configured Telegram template unavailable; using built-in fallback"
            );
            add_configured_template(environment, id, fallback)?;
            Ok(true)
        }
    }
}

#[cfg(test)]
fn ensure_source_size(source: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        source.len() <= MAX_TEMPLATE_BYTES,
        "template is too large (maximum {} bytes)",
        MAX_TEMPLATE_BYTES
    );
    Ok(())
}

fn render_loaded<S: Serialize>(
    environment: &Environment<'_>,
    id: TemplateId,
    context: &S,
) -> anyhow::Result<String> {
    let template = environment
        .get_template(id.name())
        .map_err(|error| anyhow!("template lookup failed: {}", error.kind()))?;
    let output = template
        .render(context)
        .map_err(|error| anyhow!("template rendering failed: {}", error.kind()))?;
    validate_output(&output)
}

fn validate_output(output: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !output.trim().is_empty(),
        "template rendered an empty message"
    );
    anyhow::ensure!(
        output.chars().count() <= MAX_MESSAGE_CHARS,
        "template rendered more than {} characters",
        MAX_MESSAGE_CHARS
    );
    Ok(output.to_owned())
}

#[cfg(test)]
pub(crate) fn render_alert(
    source: &str,
    hike: &tracker::Model,
    settings: &settings::Model,
    event: Option<&Event>,
    reason: &str,
    at: DateTimeUtc,
    threshold_minutes: Option<i64>,
) -> anyhow::Result<String> {
    let context = AlertContext::new(hike, settings, event, reason, at, threshold_minutes);
    render(source, &context)
}

#[cfg(test)]
pub(crate) fn render_recovery(
    source: &str,
    hike: &tracker::Model,
    settings: &settings::Model,
    event: &Event,
    at: DateTimeUtc,
) -> anyhow::Result<String> {
    let context = recovery_context(hike, settings, event, at);
    render(source, &context)
}

fn recovery_context(
    hike: &tracker::Model,
    settings: &settings::Model,
    event: &Event,
    at: DateTimeUtc,
) -> AlertContext {
    // Recovery is rendered after an OK has been accepted but before the
    // transaction writes the refreshed tracker. Build the post-OK view so a
    // recovery template sees the new last_ok_at and latest event fields.
    let mut refreshed = hike.clone();
    refreshed.last_ok_at = Some(event.event_at);
    refreshed.last_event_at = Some(
        refreshed
            .last_event_at
            .unwrap_or(event.event_at)
            .max(event.event_at),
    );
    refreshed.last_body = Some(event.body.clone());
    refreshed.location = event.location.clone();
    let mail = (event.source == IngressSource::Mail).then_some(event);
    AlertContext::new(&refreshed, settings, mail, "recovery", at, None)
}

/// Compile and render a source using a context. Rendering is intentionally
/// part of validation: it catches missing variables and bad filter arguments
/// before an owner can save a template that can never deliver an alert.
#[cfg(test)]
pub(crate) fn render<S: Serialize>(source: &str, context: &S) -> anyhow::Result<String> {
    ensure_source_size(source)?;
    let environment = configured_environment();
    let template = environment
        .template_from_str(source)
        .map_err(|error| anyhow!("template compilation failed: {}", error.kind()))?;
    let output = template
        .render(context)
        .map_err(|error| anyhow!("template rendering failed: {}", error.kind()))?;
    validate_output(&output)
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

/// Escape text inserted into a rich Markdown message. Template literals remain
/// under the author's control, while values from mail and SQLite are always
/// treated as text, including values that could otherwise be interpreted as
/// inline HTML.
pub(crate) fn markdown_escape(value: &str) -> String {
    static MARKDOWN_ESCAPE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[\\&<>_*\[\]()~`#+=\-|{}.!]").expect("Markdown escape regex is valid")
    });

    MARKDOWN_ESCAPE_RE
        .replace_all(value, |matched: &regex::Captures<'_>| {
            format!("\\{}", matched.get(0).expect("regex match exists").as_str())
        })
        .into_owned()
}

fn time_filter(value: Value, args: Rest<Value>) -> Result<Value, Error> {
    if value.is_undefined() {
        return Err(Error::new(
            ErrorKind::UndefinedError,
            "time filter received an undefined value",
        ));
    }
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    if positional.len() > 1 {
        return Err(Error::new(
            ErrorKind::TooManyArguments,
            "time filter accepts at most one format",
        ));
    }
    let positional_format = positional.first().map(|value| {
        value.as_str().map(str::to_owned).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                "time filter format must be a string",
            )
        })
    });
    let positional_format = positional_format.transpose()?;
    let keyword_format = kwargs.get::<Option<String>>("format")?;
    kwargs.assert_all_used()?;
    if positional_format.is_some() && keyword_format.is_some() {
        return Err(Error::new(
            ErrorKind::TooManyArguments,
            "time filter format was supplied twice",
        ));
    }
    let format = positional_format
        .or(keyword_format)
        .unwrap_or_else(|| "t".to_owned());
    if !valid_time_format(&format) {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "invalid Telegram time format",
        ));
    }
    if value.is_none() {
        return Ok(Value::from_safe_string(String::new()));
    }
    let seconds = value.as_i64().ok_or_else(|| {
        Error::new(
            ErrorKind::InvalidOperation,
            "time filter expects Unix seconds",
        )
    })?;
    let Some(timestamp) = chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0) else {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "time filter timestamp is out of range",
        ));
    };
    let fallback = markdown_escape(&timestamp.to_rfc3339());
    let entity = format!("![{fallback}](tg://time?unix={seconds}&format={format})");
    Ok(Value::from_safe_string(entity))
}

fn valid_time_format(format: &str) -> bool {
    static TIME_FORMAT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A(?:r|w?[dD]?[tT]?)\z").expect("Telegram time format regex is valid")
    });

    TIME_FORMAT_RE.is_match(format)
}

fn unix_seconds(value: DateTimeUtc) -> i64 {
    value.timestamp()
}

fn phase_name(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Active => "active",
        Phase::Finished => "finished",
    }
}

pub(crate) fn validate_safety_template(renderer: &Renderer, source: &str) -> anyhow::Result<()> {
    // Candidate environments are used only for editor validation. Delivery
    // always uses the startup-owned registry above.
    renderer.candidate_safety_alert(source)?.example_alerts()?;
    Ok(())
}

pub(crate) fn validate_safety_recovery_template(
    renderer: &Renderer,
    source: &str,
) -> anyhow::Result<()> {
    let settings = example_settings();
    let hike = example_hike();
    let event = example_event();
    let candidate = renderer.candidate_safety_recovery(source)?;
    candidate.render_recovery(&hike, &settings, &event, event.event_at)?;
    let telegram = example_telegram_event(&event);
    candidate.render_recovery(&hike, &settings, &telegram, telegram.event_at)?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn example_alerts(source: &str) -> anyhow::Result<(String, String)> {
    Renderer::new(source, DEFAULT_SAFETY_RECOVERY_TEMPLATE)?.example_alerts()
}

fn example_settings() -> settings::Model {
    settings::Model {
        id: 1,
        owner_reminder_minutes: crate::state::ReminderMinutes(vec![30]),
        safety_reminder_minutes: crate::state::ReminderMinutes(vec![60]),
        safety_alert_template: DEFAULT_SAFETY_ALERT_TEMPLATE.to_owned(),
        safety_recovery_template: DEFAULT_SAFETY_RECOVERY_TEMPLATE.to_owned(),
    }
}

fn example_hike() -> tracker::Model {
    let at = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000, 0).unwrap();
    tracker::Model {
        id: 1,
        phase: Phase::Active,
        started_at: Some(at),
        started_location: Some("47.3769, 8.5417".to_owned()),
        last_ok_at: Some(at),
        last_event_at: Some(at),
        last_body: Some("OK".to_owned()),
        location: Some("47.3769, 8.5417".to_owned()),
        finished_at: None,
        owner_reminders_sent: 0,
        safety_reminders_sent: 0,
        owner_alerted: false,
        safety_alerted: false,
    }
}

fn example_event() -> Event {
    let at = chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_003_600, 0).unwrap();
    Event {
        source: IngressSource::Mail,
        event_at: at,
        received_at: at,
        body: "Help: this is an example alert mail.".to_owned(),
        location: Some("47.3769, 8.5417".to_owned()),
    }
}

fn example_telegram_event(event: &Event) -> Event {
    Event {
        source: IngressSource::Telegram,
        event_at: event.event_at,
        received_at: event.received_at,
        body: "OK".to_owned(),
        location: None,
    }
}

/// Small owner templates are centralized so every owner message passes through
/// exactly the same MiniJinja formatter as configurable safety messages.
pub(crate) const OWNER_MAIN_MENU: &str = "Choose an action.";
pub(crate) const OWNER_VERSION: &str = "Version: {{ version }}\nBuild time: {{ build_time }}\nGit commit: {{ git_commit }}\nGit dirty: {{ git_dirty }}";
pub(crate) const OWNER_SETTINGS_MENU: &str = "Choose a setting.";
pub(crate) const OWNER_SETTINGS_UNAVAILABLE: &str =
    "Settings are unavailable while a hike is active.";
pub(crate) const OWNER_STARTED: &str = "InReach hike started.";
pub(crate) const OWNER_OK: &str = "OK received.";
pub(crate) const OWNER_RECOVERY: &str = "InReach contact resumed.";
pub(crate) const OWNER_FINISHED: &str = "InReach hike finished.";
pub(crate) const OWNER_FALLBACK: &str = "Safety message sent using the built-in message because the configured template could not be rendered.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_values_are_escaped_but_time_entities_are_safe() {
        let settings = example_settings();
        let hike = example_hike();
        let source = "{{ mail.body }} {{ alert.at | time(\"wDT\") }}";
        let mut event = example_event();
        event.body = "<b>& *dynamic*".to_owned();
        let output = render_alert(
            source,
            &hike,
            &settings,
            Some(&event),
            "alert_mail",
            event.event_at,
            None,
        )
        .unwrap();
        assert!(output.contains("\\<b\\>\\& \\*dynamic\\*"));
        assert!(output.contains("tg://time?unix=1700003600&format=wDT"));
    }

    #[test]
    fn markdown_escape_covers_rich_markdown_and_html_punctuation() {
        assert_eq!(
            markdown_escape(r#"\&<>_*[]()~`>#+-=|{}.! plain"#),
            r#"\\\&\<\>\_\*\[\]\(\)\~\`\>\#\+\-\=\|\{\}\.\! plain"#
        );
    }

    #[test]
    fn time_formats_accept_only_telegram_formats() {
        for value in ["", "r", "w", "d", "D", "t", "T", "wd", "wDT"] {
            assert!(valid_time_format(value), "rejected {value:?}");
        }
        for value in ["rt", "ww", "dd", "wtd", "x", "wDTx", "\n", "wDT\n"] {
            assert!(!valid_time_format(value), "accepted {value:?}");
        }
    }

    #[test]
    fn time_filter_accepts_positional_and_named_formats() {
        let settings = example_settings();
        let hike = example_hike();
        let event = example_event();
        for source in [
            "{{ alert.at | time }}",
            "{{ alert.at | time(\"wDT\") }}",
            "{{ alert.at | time(format=\"wDT\") }}",
            "{{ alert.at | time(format=\"\") }}",
        ] {
            assert!(
                render_alert(
                    source,
                    &hike,
                    &settings,
                    Some(&event),
                    "alert_mail",
                    event.event_at,
                    None,
                )
                .is_ok(),
                "failed to render {source}"
            );
        }
        assert!(
            render(
                "{{ value | time(\"rt\") }}",
                &serde_json::json!({"value": 1_700_000_000_i64})
            )
            .is_err()
        );
        let negative = render(
            "{{ value | time(\"t\") }}",
            &serde_json::json!({"value": -1_i64}),
        )
        .unwrap();
        assert!(negative.contains("unix=-1&format=t"));
    }

    #[test]
    fn missing_values_are_strict_but_none_is_renderable() {
        let settings = example_settings();
        let hike = example_hike();
        let event = example_event();
        assert!(
            render_alert(
                "{{ missing }}",
                &hike,
                &settings,
                Some(&event),
                "alert_mail",
                event.event_at,
                None
            )
            .is_err()
        );
        assert!(
            render_alert(
                "X{{ hike.finished_at }}",
                &hike,
                &settings,
                Some(&event),
                "alert_mail",
                event.event_at,
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn context_distinguishes_alert_mail_and_overdue_values() {
        let settings = example_settings();
        let hike = example_hike();
        let event = example_event();
        let mail = serde_json::to_value(AlertContext::new(
            &hike,
            &settings,
            Some(&event),
            "alert_mail",
            event.event_at,
            None,
        ))
        .unwrap();
        assert_eq!(mail["alert"]["reason"], "alert_mail");
        assert_eq!(mail["alert"]["threshold_minutes"], serde_json::Value::Null);
        assert_eq!(mail["alert"]["deadline_at"], serde_json::Value::Null);
        assert_eq!(mail["mail"]["event_at"], 1_700_003_600_i64);
        assert_eq!(mail["mail"]["received_at"], 1_700_003_600_i64);

        let overdue = serde_json::to_value(AlertContext::new(
            &hike,
            &settings,
            None,
            "overdue",
            event.event_at,
            Some(60),
        ))
        .unwrap();
        assert_eq!(overdue["mail"], serde_json::Value::Null);
        assert_eq!(overdue["alert"]["threshold_minutes"], 60);
        assert_eq!(overdue["alert"]["deadline_at"], 1_700_003_600_i64);
        assert_eq!(overdue["alert"]["elapsed_minutes"], 60);
        assert_eq!(overdue["alert"]["minutes_since_last_ok"], 60);
        assert_eq!(overdue["hike"]["last_ok_at"], 1_700_000_000_i64);
    }

    #[test]
    fn recovery_context_reflects_the_new_ok_and_only_mail_events_fill_mail() {
        let settings = example_settings();
        let hike = example_hike();
        let event = example_event();
        let output = render_recovery(
            "{{ alert.reason }} {{ hike.last_ok_at }} {{ mail.body }}",
            &hike,
            &settings,
            &event,
            event.event_at,
        )
        .unwrap();
        assert!(output.contains("recovery 1700003600"));
        assert!(output.contains("Help: this is an example alert mail\\."));

        let telegram = example_telegram_event(&event);
        let output = render_recovery(
            "{{ alert.reason }} {{ hike.last_ok_at }}{% if mail is not none %} {{ mail.body }}{% endif %}",
            &hike,
            &settings,
            &telegram,
            telegram.event_at,
        )
        .unwrap();
        assert!(output.contains("recovery 1700003600"));

        let renderer = Renderer::defaults().unwrap();
        assert!(validate_safety_recovery_template(&renderer, "X{{ mail.body }}").is_err());
    }

    #[test]
    fn rendering_rejects_empty_and_oversized_output() {
        let settings = example_settings();
        let hike = example_hike();
        let event = example_event();
        assert!(
            render_alert(
                "   ",
                &hike,
                &settings,
                Some(&event),
                "alert_mail",
                event.event_at,
                None
            )
            .is_err()
        );
        assert!(
            render_alert(
                &"x".repeat(MAX_MESSAGE_CHARS + 1),
                &hike,
                &settings,
                Some(&event),
                "alert_mail",
                event.event_at,
                None
            )
            .is_err()
        );
        assert!(
            render_alert(
                &"x".repeat(MAX_TEMPLATE_BYTES + 1),
                &hike,
                &settings,
                Some(&event),
                "alert_mail",
                event.event_at,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn rich_message_limit_counts_unicode_characters() {
        let settings = example_settings();
        let hike = example_hike();
        let mut event = example_event();
        event.body = "🙂".repeat(MAX_MESSAGE_CHARS);
        assert!(
            render_alert(
                "{{ mail.body }}",
                &hike,
                &settings,
                Some(&event),
                "alert_mail",
                event.event_at,
                None,
            )
            .is_ok()
        );
        event.body.push('🙂');
        assert!(
            render_alert(
                "{{ mail.body }}",
                &hike,
                &settings,
                Some(&event),
                "alert_mail",
                event.event_at,
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn default_template_handles_both_alert_causes() {
        let (mail, overdue) = example_alerts(DEFAULT_SAFETY_ALERT_TEMPLATE).unwrap();
        assert!(mail.contains("alert\\_mail"));
        assert!(overdue.contains("overdue"));
    }

    #[test]
    fn renderer_selects_registered_templates_by_id_and_refreshes_sources() {
        let renderer = Renderer::defaults().unwrap();
        assert_eq!(
            renderer
                .render(TemplateId::OwnerMainMenu, &json!({}))
                .unwrap(),
            "Choose an action."
        );
        renderer
            .set_safety_alert_source("{{ alert.reason }}")
            .unwrap();
        let settings = example_settings();
        let hike = example_hike();
        let event = example_event();
        assert_eq!(
            renderer
                .render_alert(
                    &hike,
                    &settings,
                    Some(&event),
                    "alert_mail",
                    event.event_at,
                    None
                )
                .unwrap(),
            "alert\\_mail"
        );
    }

    #[test]
    fn guide_mentions_context_and_filter_rules() {
        let guide = Renderer::defaults().unwrap().guide().unwrap();
        assert!(guide.contains("MiniJinja"));
        assert!(guide.contains("rich Markdown"));
        assert!(guide.contains("wDT"));
        assert!(guide.contains("last_ok_at"));
        assert!(guide.contains("{{ alert.reason }}"));
        assert!(guide.chars().count() <= MAX_MESSAGE_CHARS);
    }
}
