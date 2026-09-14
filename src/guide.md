# Safety message templates

You can edit the alert and recovery messages separately. A template is an
ordinary Telegram message with a few tracker fields filled in for you.

## Alert

Template source:

{% raw %}```jinja2
{% if last_alert is not none %}**SAFETY ALERT:** {{ last_alert }}{% else %}**SAFETY ALERT:** No OK has been received since {{ last_ok_at }}.{% endif %}

*Here are details to help locate the hiker.*

- **Hike started:** {{ started_at | date }} at {{ started_at | time }}
- **Starting location:** {% if started_location is not none %}{{ started_location }}{% else %}not recorded{% endif %}
- **Most recent event:** {{ last_event_at }}
- **Last OK:** {{ last_ok_at }}
- **Last known location:** {% if location is not none %}{{ location }}{% else %}not recorded{% endif %}

*Contact the hiker and use the locations above to decide what to do next.*
```{% endraw %}

---

Telegram renders that source like this:

{% if alert.last_alert is not none %}**SAFETY ALERT:** {{ alert.last_alert }}{% else %}**SAFETY ALERT:** No OK has been received since {{ alert.last_ok_at }}.{% endif %}

*Here are details to help locate the hiker.*

- **Hike started:** {{ alert.started_at | date }} at {{ alert.started_at | time }}
- **Starting location:** {% if alert.started_location is not none %}{{ alert.started_location }}{% else %}not recorded{% endif %}
- **Most recent event:** {{ alert.last_event_at }}
- **Last OK:** {{ alert.last_ok_at }}
- **Last known location:** {% if alert.location is not none %}{{ alert.location }}{% else %}not recorded{% endif %}

*Contact the hiker and use the locations above to decide what to do next.*

## Recovery

Template source:

{% raw %}```jinja2
{% if active %}**SAFETY CONTACT RESUMED:** The hike continues normally.{% else %}**SAFETY CONTACT RESUMED:** The hike was finished without issues.{% endif %}
```{% endraw %}

---

Telegram renders that source like this:

{% if recovery.active %}**SAFETY CONTACT RESUMED:** The hike continues normally.{% else %}**SAFETY CONTACT RESUMED:** The hike was finished without issues.{% endif %}

## What the lines mean

1. The first alert line uses the alert mail when there is one. Otherwise it
   says how long ago the last OK was received.
2. A field between double braces is replaced with its value. The alert fields
   shown above tell when and where the hike started, when the latest event and
   OK arrived, and where the tracker was last seen.
3. The vertical bar applies a filter. `date` shows a calendar date and `time`
   shows the time of day. Unfiltered date/time fields show Telegram's relative
   time widget.
4. Locations become clickable Telegram map widgets automatically.
5. `**text**` is bold, `*text*` is italic, and a line beginning with `-` is a
   bullet. Leave an empty line between paragraphs or between the list and its
   surrounding text.
6. The recovery example uses `active`: it says that the hike continues when
   true, and that it finished without issues when false.
7. An `if` instruction chooses text conditionally. Put the instruction and its
   matching `endif` on the same line when you want a compact one-line choice.

The examples use the same sample values as the “Rendered examples” button, so
the time stays current and the map is real. Send a template as plain text or as
one single-line or multiline code block. The tracker validates it before
saving it.
