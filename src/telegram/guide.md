{% raw %}
# Telegram message template Guide

The Safety alert and Safety recovery settings are MiniJinja templates. Their
rendered result is sent as one Telegram rich Markdown message. Literal text is
rich Markdown; values inserted by `{{ ... }}` are escaped automatically and
remain ordinary text.

## MiniJinja basics

Use interpolation for a value and dot notation for nested fields:

```jinja2
{{ alert.reason }}
{{ hike.location }}
```

Strings use quotes. Comparisons use `==`, `!=`, `<`, `<=`, `>`, and `>=`.
Conditions use `{% if value %}...{% else %}...{% endif %}`. Combine conditions
with `and` and `or`. Filters use `value | filter`, with an optional argument:

```jinja2
{{ mail.body | upper }}
{{ alert.at | time("wDT") }}
```

Comments use `{# explanation #}` and do not appear in the rendered message.

Optional objects and fields are `none`. Guard them before reading a nested
value:

```jinja2
{% if mail is not none %}
Message: {{ mail.body }}
{% endif %}
{% if hike.last_ok_at is not none %}
Last OK: {{ hike.last_ok_at | time }}
{% endif %}
```

## Context variables

Every timestamp is a UTC Unix timestamp (an integer), or `none` when it is not
available. Minute values are integers. Text values are strings.

### `alert`

* `alert.reason` — the cause: `overdue`, `alert_mail`, or `recovery` (string,
  never `none`).
* `alert.at` — the effective tick time (timestamp, never `none`).
* `alert.threshold_minutes` — the overdue threshold (integer or `none`; absent
  for alert mail and recovery).
* `alert.deadline_at` — the reminder baseline plus the threshold (timestamp or
  `none`; absent when there is no threshold).
* `alert.elapsed_minutes` — whole minutes since the reminder baseline (integer
  or `none`).
* `alert.minutes_since_last_ok` — whole minutes since `hike.last_ok_at`
  (integer or `none`).

### `mail`

`mail` is `none` for overdue alerts and for recovery caused by a Telegram OK.
For an alert mail, or recovery caused by a mail OK, it is an object with:

* `mail.event_at` — the event timestamp (integer timestamp, never `none`).
* `mail.received_at` — the receipt timestamp (integer timestamp, never
  `none`).
* `mail.body` — the triggering mail body (string, never `none`).
* `mail.location` — the triggering mail location (string or `none`).

### `hike`

* `hike.phase` — `idle`, `active`, or `finished` (string, never `none`).
* `hike.started_at` — when the hike started (timestamp or `none`).
* `hike.started_location` — the initial location (string or `none`).
* `hike.last_ok_at` — the latest accepted OK and the single reminder baseline
  (timestamp or `none`). It is set when an alert mail starts a hike as if it
  were an OK and is unchanged by later alert mail.
* `hike.last_event_at` — the latest stored tracker event time (timestamp or
  `none`).
* `hike.last_body` — the latest stored tracker body (string or `none`).
* `hike.location` — the latest stored location (string or `none`).
* `hike.finished_at` — when the hike finished (timestamp or `none`).
* `hike.owner_reminders_sent` — owner reminder count (non-negative integer).
* `hike.safety_reminders_sent` — safety reminder count (non-negative integer).
* `hike.owner_alerted` — whether the owner currently has an alert (boolean).
* `hike.safety_alerted` — whether the safety chat currently has an alert
  (boolean).

### `settings`

* `settings.owner_reminder_minutes` — the configured owner reminder minutes
  (non-null list of positive, strictly increasing integers).
* `settings.safety_reminder_minutes` — the configured safety reminder minutes
  (non-null list of positive, strictly increasing integers).

## Rich Markdown

Rich Markdown supports **bold**, *italic*, `[links](https://example.test)`,
and line breaks. Escape Markdown punctuation in literal text with a backslash
when it would otherwise start formatting, for example `\*literal\*`,
`\_literal\_`, `\[literal\]`, and `\#literal`. Escape `<`, `>`, and `&` when
literal text could be interpreted as HTML. Interpolated values are escaped
automatically, including mail bodies and locations.

Keep dynamic values in ordinary text positions:

```jinja2
Location: {{ hike.location }}
Message: {{ mail.body }}
```

Do not put an interpolated value inside a code block or a link destination.
Literal links are fine:

```jinja2
[Open the tracker](https://example.test)
```

## The `time` filter

`time` accepts a UTC Unix timestamp and emits a Telegram time entity. Telegram
renders that entity in each reader's local timezone; the fallback text is only
used where the entity cannot be shown.

```jinja2
{{ alert.at | time }}
{{ alert.at | time("wDT") }}
{{ alert.at | time("r") }}
```

Without an argument the format is `t`, which shows a short local time. The
format must match `r|w?[dD]?[tT]?`. `r` shows relative time and cannot be
combined with another character. `w` shows the localized weekday. `d` shows a
short date. `D` shows a long date. `t` shows a short time. `T` shows a long
time. The empty format is valid and displays the underlying fallback text while
still carrying the timestamp. Examples include `w`, `d`, `D`, `t`, `T`, `wd`,
and `wDT`.

Guard nullable timestamps before applying the filter:

```jinja2
{% if hike.finished_at is not none %}
Finished: {{ hike.finished_at | time("wDT") }}
{% endif %}
```

## Complete copyable template

This template handles both alert causes and all optional values:

```jinja2
**SAFETY ALERT: {{ alert.reason }}**
At: {{ alert.at | time("wDT") }}
{% if alert.threshold_minutes is not none %}No OK for {{ alert.elapsed_minutes }} minutes (threshold {{ alert.threshold_minutes }}).{% endif %}
{% if hike.location is not none %}Location: {{ hike.location }}{% endif %}
{% if mail is not none and mail.location is not none %}Mail location: {{ mail.location }}{% endif %}
{% if mail is not none %}Message:
{{ mail.body }}{% endif %}
```
{% endraw %}
