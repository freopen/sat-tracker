# Migration guide: one App, one reusable Ctx

## Purpose and authority

Implement this redesign in the existing `sat-tracker` crate. This document is the
implementation specification, including the decisions made after the initial
proposal. It is not a request for another architecture proposal or an independent
rewrite crate. Preserve existing user-facing features except for the explicit
changes below.

Optimize for a small number of readable business operations, direct data access,
and tests of observable behavior. Do not introduce services, repositories,
notification intents, an outbox, transport façades, setter wrappers, or data
adapters to replace the layers being removed.

### Final decisions

- All application ownership and lifecycle orchestration belong to `App`.
- The executable only registers tracing and interrupt hooks, constructs App
  without arguments, awaits its serve method, and exits according to its Result.
- Ingress writes only inbox rows. The tick is the only runtime writer of the
  three singleton tables. Startup schema initialization is the other writer.
- **Ctx.inbox is `Option<db::inbox::Model>`.** A tick receives zero or one inbox
  row. Production immediately runs another tick when more rows remain.
- Tick processing receives only `&mut Ctx`. Row-specific business functions may
  additionally receive the original inbox row. Ctx contains no current-row index,
  separate event object, transaction, or database connection.
- Inbox is append-only during application operation. Add no inbox columns and
  never update/delete an inbox row from tick or its commit code. Processing
  advances `runtime.last_processed_inbox_id` in memory; the short singleton
  transaction commits that cursor together with all other tick state.
- Telegram calls have bounded per-call retries, including `retry_after` handling.
  An unrecovered internal error ends the process without committing the failed
  tick. The process runner owns subsequent reporting, restart, and backoff.
- Shutdown finishes the current tick, including its commit.
- `notify` has **one function visible outside its module**. It determines what is
  due by inspecting Ctx and sends notifications itself.
- There are exactly two configurable MiniJinja templates: the message SAFETY
  receives on alert and the message SAFETY receives on recovery. Other messages
  are literals or `format!` expressions, sent directly where needed.
- Every explicit alert mail produces a separate safety notification containing
  that mail's contents, even if safety was already alerted. Deduplicated delivery
  of the same Message-ID remains one input.
- Owner start, OK acknowledgement, FINISHED, and menu/settings replies remain
  direct replies at the point where their operation is processed.
- Unit tests use a shared in-memory Telegram mock. E2E uses the real executable,
  real SQLite, and the real Telegram client pointed at a local HTTP server.
- E2E transcripts use `in` and `out`, with explicit responses required for every
  `out`. Lifecycle controls exist only for persistence across restarts/crashes.
  Do not add concurrency controls or concurrency scenarios.
- A fresh database is allowed. Do not build a general legacy compatibility layer.

## 1. Ownership, lifecycle, and persistence

### App and main

The external application API is:

```rust
impl App {
    pub async fn new() -> anyhow::Result<Self>;
    pub async fn serve(self: std::sync::Arc<Self>) -> anyhow::Result<()>;
    pub fn shutdown(&self);
}
```

`new()` loads `./config.yaml` through Figment, opens `sat-tracker.sqlite`, constructs
the bot client, and initializes coordination. It starts no tasks and makes no
Telegram API calls. Keep existing config keys, regex validation, and transport
defaults. Add `listen_address`, defaulting to `0.0.0.0:8080`, for isolated test
processes. Tests select a loopback address.

Main registers tracing, constructs `Arc<App>`, installs SIGINT/SIGTERM hooks
calling `app.shutdown()`, and awaits `app.serve()`. An initialization, signal
registration, or serving error produces a failing process exit. Version/startup
logging, TCP binding, routing, database work, and Telegram configuration are App
responsibilities. Interrupt registration must complete before serving starts;
retain a shutdown request received before serve begins.

App owns config, bot, database connection, shutdown/wakeup coordination, fatal
error delivery from HTTP handlers, and feature-gated test-clock coordination.
Allow `serve()` only once per App. Keep its infrastructure helpers private except
for the template-environment constructor and bounded Telegram-call helper shared
with business modules and tests.

Serve loads the singleton rows, constructs Ctx, configures Telegram, and runs
Axum, optional polling, and the tick loop under one supervisor. Webhook mode calls
`setWebhook`; polling mode calls `deleteWebhook` and then long-polls. Preserve the
existing bot request/connect timeouts. Disable opaque HTTP-client retries and
apply the bounded Telegram-call policy in section 1 instead.

Ctx lives locally inside the serving future and borrows App's config and bot.
Do not make App self-referential or put Ctx behind a shared lock. HTTP and polling
do not access Ctx.

### Ctx

```rust
pub(crate) struct Ctx<'a> {
    pub config: &'a Config,
    pub bot: &'a Bot,
    pub templates: minijinja::Environment<'static>,
    pub tracker: db::tracker::ActiveModel,
    pub runtime: db::runtime::ActiveModel,
    pub settings: db::settings::ActiveModel,
    pub inbox: Option<db::inbox::Model>,
    pub now: DateTimeUtc,
}
```

Use the existing normalized UTC timestamp type, without adding a `Now` wrapper.
Do not add `event`, `inbox_index`, pending notification commands, or duplicate
snapshots to Ctx. Raw input stays on the inbox row; reusable parsed information
belongs in tracker `last_input_*` fields. Functions may use ordinary local
variables and private pure helpers; the restriction concerns abstraction layers,
not local calculations.

App constructs the initial template environment, registers its strict-variable,
escaping, time-filter, and fuel behavior, and installs the two saved sources.
Use owned template sources so edits do not create lifetime problems. The template
names are `safety_alert` and `safety_recovery`. No other registered templates.
Use `app::template_environment(alert_source, recovery_source)` for this work and
for test-fixture initialization, so unit tests exercise the production formatter
and filters. This is a constructor, not a Ctx business-operation wrapper.

### Schema

Keep the four tables `inbox`, `tracker`, `runtime`, and `settings`, with singleton
ID 1 and their existing relevant constraints and indexes. Flatten entity modules
into `db.rs`. Flatten historical migration modules into `migration.rs`, preserving
their recorded names and independent SQL. Add one migration for the changes below;
do not silently edit the meaning of an already recorded migration.

Do not add columns to inbox. Keep its existing schema and historical rows. New
admissions have a payload and null `processed_at`, and remain that way after
processing. Neither payload clearing nor `processed_at` participates in the new
runtime algorithm. Raw payloads and deduplication keys remain retained; automated
retention/cleanup is outside this migration's scope.

Add runtime `last_processed_inbox_id BIGINT NOT NULL DEFAULT 0`, constrained to be
nonnegative. Select the next row with `id > last_processed_inbox_id ORDER BY id
LIMIT 1`, using the primary key. ID gaps are valid; the cursor is the last handled
ID, not a row count. Never filter this query by `processed_at`.

For an existing database, initialize the cursor from the greatest historically
processed ID, or zero. Verify no unprocessed row exists below that boundary and
every row above it has a payload. Reject inconsistent historical state instead of
silently skipping input; a fresh database is allowed. Migration does not rewrite
inbox rows. The existing no-history legacy reset policy can remain; document its
destructive nature. Migration failure must not trigger a reset.

Add only these parsed-input fields to tracker, all nullable initially:

| Column | Purpose |
| --- | --- |
| `last_input_kind` | `mail_ok`, `mail_finished`, `mail_alert`, or `telegram` |
| `last_input_at` | Event time needed by hike ordering and cooldown rules |
| `last_input_body` | Complete decoded mail text or Telegram text |
| `last_input_location` | Parsed location used when updating hike state |

Tick parses mail into these fields; menu parses Telegram into them. Replace their
values for every input, including ignored Telegram updates, so an old alert cannot
be mistaken for the current input. Timer-only ticks leave them unchanged. Notify
checks `ctx.inbox` as well as these fields before treating them as current input.
These fields replace a separate parsed Event object. Preserve their current values
when initializing a new hike; reset hike state fields individually.

Parse using the fixed Ctx config and the inbox receipt timestamp. Preserve MIME
decoding, regex classification, ambiguous/unknown classification as alert, date
clamping, and location extraction. Preserve valid Telegram timestamps without
mail-style future clamping; invalid/unrepresentable Telegram dates fall back to
receipt time. This replaces the old tick-time fallback with stable input data.

Do not add alert-only copies of receipt time, source time, or location. For alert
rendering use `ctx.now`, the existing inbox `received_at`, and the above parsed
fields or existing hike fields as appropriate. Keep existing template context
names where these values supply them; no extra persisted rendering metadata.

Replace tracker `owner_alerted` and `safety_alerted` booleans with nullable
`owner_alerted_for_ok_at` and `safety_alerted_for_ok_at` timestamps. A value records
the last-OK timestamp against which that audience was alerted. These fields are
durable business state, not notification commands. Convert an old true flag to
the stored `last_ok_at`, and false to null. Keep the existing reminder counters.
Update relevant constraints to use marker presence and require markers not to
exceed `last_ok_at`.
Rebuild the SQLite tracker table within the migration transaction as necessary
to replace the old columns and CHECK constraints, copying all other state intact.

These markers let notify distinguish a stale OK from actual recovery after a
newer OK without another event/snapshot object. The safety template context still
exposes the existing `owner_alerted` and `safety_alerted` boolean names, derived
from marker presence. Do not change the template language's public context schema.

### Admission

Preserve `/mail`, `/tg`, `/healthz`, the 1 MiB request limit, and existing HTTP
validation behavior. Do not add a new authentication scheme in this redesign.
Owner-chat filtering remains mandatory during Telegram business processing.

Insert the original inbox input durably before returning 204. Only after commit
notify the scheduler. Admission must not write runtime deadlines, polling offset,
menu position, settings, or hike state.

Use the existing source/external-ID uniqueness constraint for deduplication.
Never overwrite a duplicate's original payload. Mail without Message-ID always
creates a new row. Unsupported Telegram updates can remain
ordinary rows that the tick ignores and advances past using the runtime cursor.

Polling has a process-local cursor initialized from the greatest durably admitted
Telegram update ID across pending and processed rows, plus one, or zero if none.
Advance it only after successful admission. The tick may keep the existing
persisted runtime polling offset current as it processes rows, but the poller
must not depend on that offset keeping pace with admission. No polling write to
runtime is allowed.

### Tick and commit

Production performs an initial tick. On each iteration App sets a nondecreasing,
millisecond-normalized time and loads the first row above the committed runtime
cursor by ID with `LIMIT 1`, storing the query result directly in Ctx.inbox.
The persisted last-tick timestamp is the lower bound across restart.

`tick(&mut ctx)` processes the optional input and then calls `notify::notify(ctx)`
once. The current row can be cloned into a local value to avoid borrowing Ctx's
Option while passing `&mut Ctx`; leave the row in Ctx for notify. Do not create a
second Event type or mutate the row into a notification instruction. After the
selected input handler succeeds, tick advances the in-memory runtime cursor to
that row's ID, including ignored/unauthorized inputs. A subsequent notification
failure still prevents that cursor from being committed. A no-input tick does
not change the cursor.

Tick does not perform database I/O. It updates the last-tick time, while notify
calculates the next notification deadline. After tick succeeds, App:

1. Opens a short write transaction.
2. Writes singleton fields marked changed in their ActiveModels, including the
   runtime cursor. It performs no inbox mutation.
3. Commits.
4. Retains complete singleton values with change tracking reset to Unchanged,
   sets Ctx.inbox to None, and reuses the same Ctx and environment.

Never begin the write transaction before Telegram processing. Assign modified
ActiveModel fields with `.set_ne(value)` when updating existing values. SeaORM
2.0.2 supports this directly: an equal value preserves its existing tracking and
a different value becomes Set. Use Set for initial construction where appropriate;
never mutate a value while leaving a real change marked Unchanged. If an ORM
update consumes the model, use its returned complete model to rebuild the clean
ActiveModel only after successful transaction commit.

New admissions are not part of the current commit. Query for more pending input
before sleeping and immediately run another tick if present. Otherwise wait for
ingress, shutdown, or the next deadline. Use normal Tokio notification semantics
without losing a wakeup between checking work and sleeping.

Each tick checks deadlines after its own input, even if another input is queued.
There is no lookahead across pending rows: an overdue alert can therefore precede
an OK in the following tick. This is an intentional consequence of the final
one-entry-per-tick decision, replacing the old all-pending-before-reminders rule.

### Failure and shutdown

Unexpected initialization, polling, serving, admission-storage, processing, or
commit errors that remain after the Telegram retry policy end serve with an error.
Do not retry database operations or rerun a whole tick in process.
Malformed input and invalid owner settings/templates are handled rejections.
Preserve recognized template rendering/Telegram-formatting fallback behavior;
permanent and exhausted transient Telegram errors propagate.

Use one small shared `telegram_call` async helper around the actual bot call. It
accepts a closure creating a fresh attempt and returns the bot method's typed
Result. This is the sole transport-policy exception to direct calls, not a bot
facade: request construction and business decisions stay at their call sites.

The default policy is fixed and bounded:

- Three attempts total: the initial attempt and at most two retries.
- Retry network connection/timeouts, HTTP 429/5xx, and Telegram API error codes
  429/5xx. Inspect response parameters for Telegram `retry_after`.
- Delay 1 second before retry one and 2 seconds before retry two. For either,
  wait at least `retry_after` seconds when supplied. Never cap that value and then
  retry earlier. Failure to represent its deadline safely returns an error.
- Do not retry other 4xx errors, JSON/schema decoding failures, or configuration
  errors. Recognized rich-message rejection goes directly to existing fallback
  handling. Fallback calls themselves use the same bounded policy.
- Retry only the failing API call with the same request parameters. Do not repeat
  earlier successful calls, reparse the input, or increment counters on failed
  attempts. A timed-out call may have succeeded remotely, so duplicates remain
  possible even with this smaller retry boundary.
- Use the helper for webhook setup, polling calls, previews, and all sends. Reset
  the attempt budget per API call. After exhaustion the existing fatal path runs;
  polling must not add another unbounded retry loop.

Retry delays use Tokio monotonic timers, not Ctx.now. Do not advance business time
mid-tick. Shutdown still lets an active tick finish, including its bounded retries
and any server-requested wait. Non-tick setup/polling can be cancelled on shutdown.
Do not stack additional reqwest automatic retries beneath this helper.

On tick failure, do not commit any of its changes or try to restore Ctx for reuse.
Stop sibling services and discard it. Telegram messages already delivered cannot
be rolled back and can be repeated after restart. A permanently failing row can
cause a restart loop; do not introduce automatic skipping or quarantine.

On shutdown, stop starting ticks/polls, close HTTP admission gracefully, finish
admission handlers already running, and let the current tick finish and commit.
Later admitted inputs remain pending for restart. Await service cleanup before
returning. If a sibling fails during a running tick, finish that tick normally
when it can succeed; a failure of that tick itself prevents its commit.

Log safe operation names, inbox IDs, API status codes, and available retry-after
metadata. Never log tokens, token-bearing URLs, or message bodies. Do not let
main's returned error expose a raw Telegram error containing private data.
Process-runner configuration is external; document the required restart/backoff
assumption without claiming repository changes configure it.

## 2. Business modules and notifications

### Hike operations

Expose distinct `receive_ok`, `finish_hike`, and `receive_alert` operations taking
Ctx only, reading parsed input from tracker. A private `start_hike` helper is a
legitimate shared business operation; do not add a separate public wrapper around it.

Preserve existing event-order checks, phase transitions, timestamps, location
updates, five-minute mail-finished cooldown boundary, and Telegram's exemption
from that cooldown. Use tracker `last_input_at`, `last_input_body`, and
`last_input_location`, populated from the selected raw inbox row.

On a newer OK, update hike state and reset reminder counters, but retain audience
alert markers until notify has delivered the applicable recovery. On a stale OK,
do not refresh state or reset counters. Preserve the quiet owner acknowledgement
for processed OK events, except mail rejected by the finished cooldown.

Start, OK acknowledgement, and FINISHED owner replies use hardcoded text and the
appropriate keyboard at their processing sites. New-hike initialization clears
old counters and markers. FINISHED does not send safety recovery.

An alert mail starts an inactive hike as before, including its owner start/OK
replies. For an active hike it does not refresh last OK or overwrite the last OK
body/location. Notify reads its decoded text from tracker.last_input_body and
requires a current mail inbox row, so no pending-alert instruction is needed.

### Menu

One public entry point processes an owner Telegram row. It handles authorization,
message/command interpretation, settings navigation, validation, updates, and
direct bot replies. It invokes hike operations for hike commands.

Preserve `/start`, `/version`, keyboard labels and options, active/inactive
settings restrictions, reminder-list validation, Back behavior, template editors,
rendered examples, and the guide. Active hike state resets menu position to Main
after either mail or Telegram input.

Only confirmed settings belong in settings; menu position belongs in runtime.
Use literals/`format!` and construct bot request parameters directly. Private pure
helpers may validate reminder lists, validate candidate templates, escape text,
or build keyboards. Do not add `set_menu_position`, generic reply forwarding, or
Ctx-based template-validation functions.

Candidate templates use a clone of the current environment. Validate and render
the same examples as today, send the required previews, then replace the saved
source and environment entry. Keep the existing acceptance reply. A send failure
remaining after the applicable retry policy prevents the tick's commit.

Menu owns its small sample contexts and preview rendering helpers. It may render
the two entries already in Ctx's environment directly; do not expose extra notify
functions for previews, environment construction, or validation. Preserve the
existing public template context names, Markdown escaping, `time` filter,
resource limits, and rich-message output limits. The embedded guide is hardcoded
content, not a MiniJinja template.

### Notify: one entry point

```rust
pub(crate) async fn notify(ctx: &mut Ctx<'_>) -> anyhow::Result<()>;
```

This is the only function exported by notify. All rendering, fallback, deadline,
and keyboard helpers inside the module are private. Notify does not import menu.
It constructs its own owner reply keyboard when needed.

For an inactive/finished hike, schedule no deadlines and send no automatic
recovery or reminder. For an active hike, perform these steps in order:

1. If an audience's alert marker predates the final `last_ok_at`, send recovery
   to that audience and then clear its marker. Owner recovery is hardcoded;
   safety recovery uses the configurable recovery template. Use the current row
   for mail context when it is the accepted newer mail OK; Telegram recovery has
   no mail context. Stale OK cannot qualify because last OK did not advance.
2. If Ctx.inbox contains mail and tracker.last_input_kind is `mail_alert`, send a
   safety alert using tracker.last_input_body and the available metadata. Do this
   even if safety already has an alert marker. On success, set its marker to
   current last OK and advance safety
   progress to the end of the configured schedule, suppressing scheduled safety
   reminders for this contact interval.
3. Send all remaining due owner reminders, then all remaining due safety
   reminders, in each schedule's configured order. Update the relevant counter
   and marker after each successful send. Store the earliest future deadline,
   or none when schedules are exhausted.

This uses final state after the single input. Keep simple existing overdue
semantics: a newer but already overdue OK may cause recovery followed by due
reminders in the same tick. Do not invent coalescing or historical replay logic
for this rare case. Separate alert and OK inbox rows necessarily use separate
ticks and can produce alert followed by recovery.

Retain configured-template fallback behavior and owner notices. Formatting or
size rejection can trigger the existing bounded safety fallback; permissions
failures and exhausted rate-limit/transport retries must propagate. Both the
configured explicit-mail alert and its fallback must use this mail's contents,
not the stored last-OK body. Preserve existing output bounds; do not promise
unlimited body length through Telegram.

## 3. Complete target file and public-function inventory

All production Rust files are directly under `src/`. ORM/migration namespaces
remain inline modules where required by their libraries. Public below includes
`pub(crate)`. Except for required trait implementations and compiler-generated
ORM methods, do not add functions visible across module boundaries beyond this
list. Keep unit tests beside their implementation unless listed separately.

| File | Functions visible outside the module |
| --- | --- |
| `src/main.rs` | None. Private `main` and interrupt setup only. |
| `src/lib.rs` | None. Declare modules; export App and migration support for the example. |
| `src/app.rs` | `App::new() -> Result<App>` async; `App::serve(self: Arc<Self>) -> Result<()>` async; `App::shutdown(&self)`; crate-visible `template_environment(alert_source: &str, recovery_source: &str) -> Result<Environment<'static>>`; crate-visible async `telegram_call<T, F, Fut>(attempt: F) -> anyhow::Result<T>` where `F: FnMut() -> Fut` and `Fut: Future<Output = Result<T, frankenstein::Error>>`. HTTP, admission, clock, polling, lifecycle, and commit helpers are private. |
| `src/ctx.rs` | None. Define Ctx, Bot alias, DateTimeUtc, Phase, IngressSource, InputKind (for tracker.last_input_kind), SettingsPosition, and ReminderMinutes. Preserve required serialization/ORM trait implementations. No Event, RawMail, ParsedMail, Signal, Audience, or BuildInfo wrapper. |
| `src/config.rs` | `Config::load() -> Result<Config>`. Validation/deserialization helpers private. |
| `src/db.rs` | `open(path: &Path) -> Result<DatabaseConnection>` async. Four inline entity modules, no handwritten forwarding methods. |
| `src/migration.rs` | `MigratorTrait::migrations()` and required migration `up`, `down`, `use_transaction`, and naming trait implementations. Preserve the three named historical migrations; add `m20260911_000004_processing_cursor_and_tracker_input`. |
| `src/tick.rs` | `tick(ctx: &mut Ctx<'_>) -> Result<()>` async. Parse mail or dispatch Telegram, advance the runtime cursor on handled input, reset active menu position, invoke notify, record tick time. |
| `src/hike.rs` | Async `receive_ok(ctx: &mut Ctx<'_>) -> Result<()>`, `finish_hike` and `receive_alert` with the same signature. |
| `src/menu.rs` | `process(ctx: &mut Ctx<'_>, row: &db::inbox::Model) -> Result<()>` async. |
| `src/notify.rs` | `notify(ctx: &mut Ctx<'_>) -> Result<()>` async. Exactly one visible function. |
| `src/guide.md` | No functions; embedded guide text. |
| `src/test_support.rs` | Test-only `Fixture::new()`, `Fixture::ctx(&self) -> Ctx<'_>`, mock `Bot::new()`, `Bot::calls()`, `Bot::push_response(method, response)`, and the four API methods below. |
| `tests/e2e.rs` | No public functions; one small private test per scenario pointing at its YAML file. Compile these only with feature `e2e`. |
| `tests/common/mod.rs` | `run_scenario(path: &Path) -> Result<()>` async. All transcript/process/HTTP machinery private. |
| `tests/storage.rs` | No public functions; exceptional real-SQLite checks. |
| `examples/migrate.rs` | No public functions; preserve the migration-only command. |
| `build.rs` | No public functions; preserve build metadata generation. |

The shared in-memory mock implements the called signatures of Frankenstein's
`send_rich_message`, `set_webhook`, `delete_webhook`, and `get_updates`, including
their typed Result values. Record method plus complete serialized parameters in
order. `calls()` returns a snapshot; `push_response` queues a method-specific
success or failure. Unexpected methods, exhausted scripted responses, or method
mismatches fail the test. No network or clock is used by this mock.

Use a `cfg(test)` Bot alias so Ctx unit tests use the mock and all non-unit builds
use Frankenstein's actual Bot. Adapt the two constructions in a private App
helper, not a production transport trait. The `e2e` feature is not `cfg(test)`;
the E2E child executable must use the real HTTP client.

Move mail parsing into tick's private helpers rather than retaining a public
parsing/adapter module. Admission extracts Message-ID for deduplication but does
not classify or normalize mail. Move version formatting to its actual owner reply
site and startup logging to App. Remove `src/telegram/`, `src/entity/`,
`src/migration/`, `src/http.rs`, `src/scheduler.rs`, `src/state.rs`, `src/version.rs`,
and `src/mail.rs` after their required behavior has moved. Replace the existing
`tests/scenarios.rs`, `tests/persistence.rs`, `tests/ingress.rs`, and
`tests/schema.rs` after coverage has been ported. Do not retain the old public
`App::open`, `App::tick`, ingress methods, or router API merely to keep old tests.

Other repository files:

- Update `Cargo.toml`/`Cargo.lock` for a nondefault `e2e` feature and the minimum
  required test dependencies, including YAML decoding.
- Update `.github/workflows/tests.yml` to run default and E2E-feature suites.
- Update `README.md` for lifecycle, schema, failure policy, and test commands.
- Keep `Dockerfile` a production build without `e2e`; change it only if necessary
  to make that boundary explicit.
- Keep this `MIGRATION_GUIDE.md` as the specification.
- Leave unrelated licensing, Renovate, development-environment, and container
  publication configuration unchanged. Do not add external service configuration.
- The complete YAML fixture inventory is in section 5.

## 4. E2E protocol

### Process and clock

Each scenario launches the real binary in a private temporary working directory
containing config and SQLite. Configure webhook mode and point the bot API URL at
the harness server. Never change the runner's process-wide working directory or
use the production config/database. Readiness waiting uses health checks with a
failure timeout, not fixed sleeps.

The nondefault `e2e` feature changes only time/scheduling controls:

- Test time initially equals Unix epoch, or the persisted last-tick time on restart.
- Admission queues input but does not trigger ticks. There is no startup tick or
  automatic deadline/backlog tick in this build.
- `GET /time?ts=<Unix milliseconds>` accepts an equal or increasing valid timestamp
  and requests exactly one ordinary tick, processing at most one pending input.
- Respond 204 only after the tick commits. Respond 400 for malformed, overflowing,
  or decreasing time. A tick failure produces HTTP 500 and process failure.
- Equal-time requests let a scenario process multiple pending inputs explicitly.
- Clock advancement applies to subsequent ingress receipt timestamps as well as
  the requested tick. Initialize time before feeding dated scenario inputs.
- No `/time` route exists in a production build.

`/time` handling must send its tick result back to the HTTP handler, including
failure, before graceful HTTP teardown completes. Tests can therefore assert the
failure response and then the failing process exit. Do not abort that handler
and make fixture behavior depend on a socket race.

The normal tick, notification, commit, and retry functions are shared unchanged
between production and E2E. Retry delays remain short real monotonic waits in E2E;
the `/time` barrier waits for them as part of completing the tick. Use small
`retry_after` values in fixtures. Paused Tokio time in focused helper unit tests
verifies longer waits without slowing the suite. Test-clock controls are App
infrastructure, not Ctx fields.

### YAML shape and matching

A scenario file is a top-level list. Ordinary entries contain one `in` or `out`
object. Requests have `path`, optional `headers`, optional `body`, and optional
`response` as specified below. There is no explicit `method` field.

```yaml
- out:
    path: /bottest/setWebhook
    body:
      url: https://example.test/tg
    response:
      body: {ok: true, result: true}

- in:
    path: /time?ts=1700000000000

- in:
    path: /mail
    body: |
      Message-ID: <sample>
      Content-Type: text/plain

      OK

# Processing may make outgoing calls before this request receives its response.
- in:
    path: /time?ts=1700000000000
    response:
      status: 204

# Add one out entry per actual call, with its complete expected request body
# and an explicit valid Telegram response body.
```

The example is a format illustration, not a complete runnable fixture: the last
input needs its expected outgoing messages.

Rules:

- Body absent means GET; body present means POST. Presence, not truthiness,
  determines the method: an empty string still means POST.
- A string body is literal text. A mapping or sequence body is serialized JSON;
  mappings cover normal Telegram requests. Reject ambiguous scalar/null bodies.
- JSON bodies default to `Content-Type: application/json`. Explicit headers can
  override defaults for malformed-input tests. Do not require irrelevant headers.
- For `in`, omitted response means any 2xx, with no body assertion. An explicit
  response may assert status, selected headers, and body. If explicit status is
  omitted, it also means any 2xx.
- **Every `out` must contain `response`.** Its status defaults to 200. Body and
  headers define the literal server reply; no method-specific result is invented.
  A missing response body means an empty HTTP body, not a Telegram success body.
  Normal fixtures provide a valid typed Telegram `result` themselves.
- Compare full JSON bodies structurally and text bodies exactly. Compare method,
  path/query, and only the specified meaningful headers, case-insensitively by
  header name. Do not record transport framing or incidental client headers.
- Support explicit build metadata placeholders for version output, resolved from
  the tested build. Do not wildcard message contents, chat IDs, or keyboards.
- A declared `out` consumes exactly the next outbound call and supplies its
  response. Unexpected, missing, reordered, or surplus calls fail the scenario.

An `in` launches its request without waiting before reading immediately following
`out` entries. Service those outputs in order, then await and validate the input
response before the next `in`, lifecycle control, or end of file. This permits
synchronous bot calls within `/time` without a separate request/response DSL.
It does not introduce concurrent input scenarios or held-response controls.

Outgoing startup calls may precede the first input. Start App automatically at
scenario beginning. At scenario end, verify all expected exchanges were consumed,
shut down gracefully, and reject extra outbound calls. Use timeouts only to fail
a stuck test, not to determine application behavior or expected ordering.

### Persistence lifecycle controls

Allow three additional entries:

```yaml
- restart: true
- crash: true
- exit: failure
```

`restart` gracefully stops a running child (requiring successful exit) and starts
a new one using the same directory/database. If the previous child has already
exited as expected, just start the replacement. Assert its startup Telegram calls
with ordinary following `out` entries.

`crash` kills the child abruptly and waits for termination, preserving files. It
is used only between completed exchanges to test durability across process loss.
Use `restart` to launch again. `exit: failure` waits for App to fail following a
scripted error; it does not kill it. Use this only to verify failed-tick persistence
and replay. Do not test independent crash behavior, runner backoff, or supervision
internals that have no persistence assertion.

No response holds/releases, concurrent request controls, or concurrency fixtures.

## 5. Tests and acceptance scenarios

### Ctx unit tests

The shared Fixture owns config and the in-memory Bot. Its `ctx()` constructs
complete singleton ActiveModels and a real two-template environment, borrowing
fixture values and using App's shared environment constructor. Tests then set
fields directly, including a raw optional inbox row and tracker.last_input_*
fields for tests that call hike/notify directly. Tests of tick/menu supply raw
input and assert those parsed tracker fields as part of the resulting state.

Call public business operations and assert meaningful state changes plus ordered
Telegram calls: recipient, full text, reply keyboard, notification options, and
failure propagation. Verify fields that must remain unchanged where relevant.
Do not test forwarding wrappers, individual assignments, or implementation shape.

In particular test stale OK versus newer OK recovery using alert markers, every
explicit alert mail despite an existing marker, phase/command authorization,
template candidate acceptance, and error paths after an earlier successful send.

### E2E fixture inventory

Create exactly these initial fixtures under `tests/scenarios/`, with one thin
test entry per file. Cases listed together may share a lifecycle where it is
meaningful, but do not combine unrelated assertions merely to reduce test count.

| File | Required scenarios |
| --- | --- |
| `startup.yaml` | Config-file loading, webhook registration, health, clean startup/shutdown. |
| `http_validation.yaml` | Empty mail, malformed Telegram JSON, request limit, normal 204 admission. |
| `owner_commands.yaml` | `/start`, `/version`, all hike command forms, unknown/nontext/nonmessage updates, non-owner commands/settings ignored, keyboard and quiet OK behavior. |
| `hike_lifecycle.yaml` | Start, refresh, owner reminder, safety reminder, recipient-specific recovery, finish, inactive behavior. |
| `stale_and_cooldown.yaml` | Stale OK and FINISHED, exact five-minute mail cooldown boundary, Telegram bypass, stale OK does not create recovery. |
| `mail_parsing.yaml` | MIME/quoted-printable body, valid/missing/invalid/future mail date, location extraction, correct parsed tracker fields and notification content. |
| `explicit_alerts.yaml` | Unknown and ambiguous mail; inactive start; two distinct alert mails each notify with their own contents; last OK unchanged; safety schedule suppression; later OK recovery. |
| `reminder_schedules.yaml` | Multiple configured thresholds, exact boundaries, overdue catch-up, audience ordering, exhausted schedules, counter reset after newer OK. |
| `recipient_recovery.yaml` | Owner-only, safety-only, and both-audience recovery; no recovery without a prior alert; safety recovery uses its template. |
| `one_input_per_tick.yaml` | Queue multiple inputs before ticking; each equal-time `/time` processes only the oldest one; effects and acknowledgements appear separately; a zero-input tick still checks deadlines. |
| `input_before_deadline.yaml` | The selected OK or FINISHED precedes that tick's reminder checks; a following queued OK does not suppress the previous tick's due reminder. |
| `deduplication.yaml` | Mail and Telegram duplicates with changed payloads, duplicates after processing and restart, no-ID mail remains separate input. |
| `settings.yaml` | Both reminder editors; valid and invalid lists; navigation; active-phase refusal; only confirmed values stored; mail start resets editor position. |
| `template_editing.yaml` | Both safety editors, examples, guide, acceptance, invalid syntax/variables/empty or oversized output, Telegram sample rejection, subsequent use of the accepted source. |
| `template_fallback.yaml` | Safety alert and recovery rendering failures and recognized Telegram format rejection; bounded fallback and owner notice; correct current alert-mail contents. |
| `rich_messages.yaml` | Escaped values, time entities, Unicode, messages beyond the legacy text limit, bounded long-message fallback. |
| `restart_state.yaml` | Hike, parsed tracker input, settings, menu position, both templates, notification markers/counters, processing cursor, and deduplication survive restart; downtime counts after `/time` advancement. |
| `crash_pending_input.yaml` | Admit input, crash before ticking, restart, process the row above the cursor exactly once; repeat after committed processing to verify retained inbox payloads are not replayed. |
| `telegram_retries.yaml` | Transient send failure then success, explicit retry_after response, no replay of earlier successful sends, and successful cursor/state commit. Every attempt has its own out and explicit response. |
| `failed_tick_replay.yaml` | A later call exhausts its retries after an earlier send succeeds; process exits, restart replays the row above the unchanged cursor; no partially saved hike/menu state. Include a reminder failure after processing the tick's one input. |
| `failed_template_update.yaml` | Failure during previews or final acceptance reply; restart keeps the old confirmed template and pending row; a successful replay saves the replacement. |
| `test_clock.yaml` | Equal/increasing timestamps, invalid/overflow/backward rejection, one-tick barrier behavior, persisted lower bound on restart. |

There is no later-inbox-event rollback case within a tick: that architecture has
been removed. There is no explicit-alert suppression fixture: distinct alert mails
must now all be delivered. Every outgoing fixture exchange specifies its response,
including webhook setup and successful message sends.

### Exceptional tests

Keep only a few tests where the above levels cannot establish the property:

- Real SQLite failure while committing singleton changes and the processing
  cursor must roll both back together, with the inbox row entirely unchanged.
  Inject failure through the database in the focused storage test, not a
  production HTTP failure-injection endpoint.
- Two successive successful commits retain all ActiveModel values and persist
  changes made on the second tick; a failed commit never publishes cleaned models
  as reusable state.
- Schema constraints, unique identifiers, unchanged retained inbox rows, cursor
  advancement across ignored inputs and ID gaps, and migration
  application/rejection need small storage tests.
- Polling cursor initialization/advancement can use the in-memory mock and a
  temporary database. E2E stays webhook-only.
- A production-build HTTP smoke check verifies `/time` is absent. Test the pure
  nondecreasing clock/deadline calculation if it remains nontrivial.
- Keep pure tests for template escaping, fuel/output bounds, and safe error
  formatting only where they catch critical cases not adequately covered above.
- Test the retry helper's three-attempt bound, transient/permanent classification,
  `retry_after` floor, and unchanged request parameters with paused Tokio time.
  This tests our retry policy, not Tokio scheduling internals.

Tests needing private App commit/polling helpers live in `app.rs` under `cfg(test)`;
do not export those helpers for `tests/storage.rs`. The latter uses migration
support and direct SQLite access for schema-level assertions.

Do not add concurrency, Tokio/Axum scheduling, load, or shutdown-race tests. Do not
retain the old broad integration suites beside their replacements.

## 6. Implementation sequence and completion criteria

1. Read current code/tests and record the behavior mapping to the fixture inventory.
2. Flatten storage/migrations, add the runtime cursor, parsed tracker fields and
   alert markers, and introduce optional-input Ctx and its in-memory test fixture.
3. Move business behavior into hike, menu, and the single notify entry point.
   Remove forwarding layers as each operation moves.
4. Implement App-owned serving, inbox-only admission, one-entry ticks, short
   singleton commits, bounded Telegram-call retries, error propagation, and shutdown.
5. Implement the feature-gated clock and shared YAML runner. Port scenarios before
   deleting their old coverage.
6. Remove obsolete files/APIs, update README and CI, and perform verification.

Do not expand the public-function inventory to solve borrow-checker friction.
Use a local clone of the optional input row, ActiveModel `.set_ne()`, and private
helpers. If an actual library/environment constraint makes this design impractical,
surface it before investing in a workaround, as requested by the repository owner.

Check Cargo availability in the actual process before selecting commands. Run
checks sequentially, not concurrently against the same target directory:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo test --workspace --locked --features e2e
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
git diff --check
```

Compile the actual E2E child with feature `e2e`, not just its test harness. Keep
production builds on default features. CI must run both modes.

Completion requires the specified file/function boundaries, successful scenario
coverage, no database access from tick/Ctx business functions, no runtime singleton
writes outside the tick commit, no tick-time inbox mutations, explicit out
responses everywhere, and bounded per-call retries without whole-tick retries.
Report commands actually run and unresolved failures honestly. Do not
claim exactly-once Telegram delivery or externally configured runner backoff.
