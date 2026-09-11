# sat-tracker

A small InReach hike monitor with Telegram owner and safety notifications.

## Processing

SQLite is the authoritative store for the inbox, current hike, confirmed bot
settings, Telegram polling offset, settings-menu position, and tick deadlines.
SeaORM migrations apply versioned schema changes at startup and record them in
`seaql_migrations`. There is no action engine or outbox.

Ingress commits an inbox entry and an ASAP tick deadline before acknowledging
receipt. Telegram updates are deduplicated by update ID, mail by Message-ID.
Mail without Message-ID is accepted as a separate event on each delivery.
Processed payloads are cleared; identifiers remain for deduplication.

One scheduler calls `App::tick(DateTime<Utc>)`. Each call begins an SQLite
`IMMEDIATE` transaction, processes the inbox in order, sends Telegram messages
inline, checks reminder deadlines, and commits state and the next deadline.
Ingress waits behind that transaction for up to five seconds, then returns an
error so its sender can retry. Telegram requests have a 40-second timeout.

Any processing or send failure rolls back the entire tick. Telegram messages
already delivered cannot be rolled back and can be repeated on retry. The
scheduler retries after five seconds, or Telegram's longer `retry_after`.
Messages are sent in one Telegram Rich Message API call. Rich message text can
contain up to 32,768 UTF-8 characters.

The scheduler supplies the maximum of the UTC wall time, its previous attempted
tick time, and the scheduled deadline when a timer fires. Persisted timestamps
use UTC `DateTime` values normalized to millisecond precision. Committed tick
times persist across restarts; failed attempt times remain in the scheduler
until restart. Downtime counts toward deadlines.
Tests can use `App::open`, ingress methods, and explicit calls to `tick` without
starting background tasks. `Arc<App>::run` starts scheduling and configures
Telegram webhook delivery or long polling; `shutdown` stops these tasks.

An OK starts or refreshes a hike. By default, the owner is reminded after 30
minutes and safety after 60 minutes without a newer OK. Reminder schedules can
be edited by the owner from Telegram while no hike is active. A later OK sends
a recovery message to the owner when the owner was alerted. FINISHED notifies
the owner. Mail OK events received within five minutes of FINISHED are ignored
to protect against delayed or out-of-order mail; Telegram actions are not
subject to this mail safeguard.
Alert or ambiguous mail starts an inactive hike exactly like an OK immediately
followed by an alert mail, then sends the configured safety alert. Further
alert mail leaves `last_ok_at` unchanged and is suppressed while safety is
already alerted. The safety chat receives alerts for the two configured causes;
the alert mail also suppresses that interval's scheduled safety reminders. A
newer OK clears both audiences' alerted state and sends a
recovery message to each audience that was alerted; safety recovery wording is
configured separately from the safety alert wording. FINISHED sends only the
owner message.

Only the configured owner chat can use `/start`, `/version`, and the reply
keyboard. `/start` shows a persistent reply keyboard with `Start hike` and
`Settings` when inactive, or `OK` and `FINISHED` when active. Settings contains
separate owner and safety reminder-time entries plus `Safety alert template` and
`Safety recovery template`.
Each reminder value is entered as a non-empty, strictly increasing
comma-separated list of positive minutes, such as `30, 45, 60`. The alert
template editors offer rendered examples and a Guide covering MiniJinja and
Telegram rich Markdown. Alert templates are rendered for both alert causes;
recovery templates are rendered for a recovery example. Samples are sent to
the owner before a replacement is saved. Message sources share one MiniJinja
environment initialized at startup; changing a configurable source refreshes
its registered entry. The Guide source lives in `src/telegram/guide.md`, is
embedded into the binary at build time, and is sent as one rich message. The
keyboard is attached to owner notifications and every accepted OK also sends a
silent `OK received.` owner message to restore it.

## Configuration and deployment

Configuration is loaded from `./config.yaml` using Figment. Keep this file private:

```yaml
telegram_bot_token: "YOUR_TOKEN"
owner_chat_id: 123
safety_chat_id: 456
ok_regex: 'ALL OK'
finished_regex: 'FINISHED'
# Omit or leave empty to use long polling:
telegram_webhook_url: ''
# Optional; defaults to https://api.telegram.org:
telegram_api_url: 'https://api.telegram.org'
```

The server listens on port 8080. POST raw mail to `/mail`, POST Telegram updates
to `/tg`, and GET `/healthz` for liveness. Request bodies are limited to 1 MiB.
Both ingress endpoints return 204 after commit, or an error when acceptance fails.

Run exactly one application instance per database. The process needs write
access to its working directory for `sat-tracker.sqlite` and its WAL files.

Startup treats a database without a `seaql_migrations` table as legacy and
**automatically deletes its tables and data**, then applies the initial migration.
This includes both the old durable-actions and entity-first databases. Stop the
old instance before upgrading; existing hike state is intentionally discarded.
Once migration history exists, startup applies only pending migrations. A
migration error stops startup and never triggers an automatic reset.

Schema changes belong in new files under `src/migration/`, registered in
`Migrator::migrations()`. Each migration defines its own SQL independently of the
runtime entities, including `up` and `down`. Update entity mappings alongside new
migrations; do not edit migrations that have already been applied. The initial
migration creates the tracker and runtime singleton rows; later migrations add
confirmed settings, the runtime settings-menu position, both safety message
templates, and the template-editor positions.

The schema enforces singleton IDs, known sources and hike phases, inbox payload
lifecycle, nonnegative counters, boolean values, and consistent hike timestamps.
The inbox has a source/external-ID uniqueness constraint and a partial index for
pending rows in arrival order. Tracker and runtime queries use their primary keys.

To apply migrations locally without starting the bot:

```sh
cargo run --locked --example migrate
```

`src/entity` contains hand-written entities. They map storage columns to native
Rust types such as `Phase` and `IngressSource`; constraints and indexes remain
owned by the versioned migrations. When intentionally rebuilding a disposable
database after changing the initial migration, stop the app and remove
`sat-tracker.sqlite` and any WAL/SHM sidecars before running the migration command.
Normal upgrades add new migrations and update the entity mappings together.

## Source and extension points

- `app.rs`, `http.rs`, `telegram.rs`: ingress, lifecycle, and transport.
- `scheduler.rs`, `tick.rs`: scheduling and the transactional procedure.
- `state.rs`, `telegram/template.rs`, `mail.rs`: domain types, rendering, and mail parsing.
- `db.rs`, `migration/`, `entity/`: SQLite setup, versioned migrations, and entity mappings.

Reminder thresholds are ordered lists with per-audience progress counters,
initially `[30]` and `[60]`. Confirmed schedules are stored as JSON-backed
`ReminderMinutes` values in the singleton settings entity and are updated during
the transactional tick. The current owner settings-menu position is stored in
the runtime entity, so a prompt can survive a restart. Garmin polling can add
persisted GPS data and a next-poll deadline to the same procedure, with
expected polling failures rescheduled without aborting safety processing.

## Verification

```sh
cargo fmt --check
cargo test --workspace --locked
cargo clippy --all-targets --all-features --locked -- -D warnings
git diff --check
```

Integration tests use temporary SQLite files and a fake Telegram server; they
never contact the production bot or open the production database.
