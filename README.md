# sat-tracker

`sat-tracker` monitors an InReach hike. It accepts status messages by mail or
Telegram, keeps the hike state in SQLite, reminds the owner when an OK is late,
and alerts a safety chat when the configured safety deadline is missed.

## Architecture

The application has one owner: `App`. It owns the database connection, the
Telegram client, HTTP admission, polling or webhook setup, shutdown, cleanup,
and the tick supervisor. A single owned `Ctx` lives inside `App::serve`; it
contains the singleton ActiveModels, templates, current time, and at most one
inbox row. Business operations receive `&mut Ctx` and update that context
directly.

The normal data flow is:

```text
HTTP request or Telegram poll
        -> append one inbox row and commit it
        -> wake the tick loop
        -> process the oldest row after the runtime cursor
        -> evaluate reminders and alerts
        -> commit changed tracker, runtime, and settings rows
```

Each tick handles at most one inbox row. The inbox row is never changed by
processing: its payload, source, ID, and receipt time remain available for
deduplication and diagnostics. The runtime cursor records which row was handled,
including ignored or unauthorized inputs. A failed tick is not committed.

## Source layout

All application modules are directly under `src/`:

| File | Responsibility |
| --- | --- |
| [`app.rs`](src/app.rs) | `App`, `Ctx`, HTTP routes, durable admission, tick processing, singleton commits, cleanup, and task supervision. |
| [`bot.rs`](src/bot.rs) | Thin retrying wrapper around Frankenstein’s reqwest bot; the test build contains the Mockall Telegram bot. |
| [`config.rs`](src/config.rs) | YAML configuration, defaults, and regex validation. |
| [`db.rs`](src/db.rs) | SeaORM entities, database value types, and SQLite connection/migration startup. |
| [`hike.rs`](src/hike.rs) | Start, OK, and finish operations plus the owner’s direct replies. |
| [`mail.rs`](src/mail.rs) | MIME decoding, mail timestamps, location extraction, and OK/FINISHED/alert classification. |
| [`menu.rs`](src/menu.rs) | Owner command matching, settings screens, reply keyboards, and template editing. |
| [`migration.rs`](src/migration.rs) | SeaORM migration history and migration tests. |
| [`notify.rs`](src/notify.rs) | Due owner reminders, safety reminders, explicit safety alerts, and safety recovery. |
| [`template.rs`](src/template.rs) | Strict MiniJinja environment, template validation, Telegram Markdown formatting, time/map widgets, and rendered guide examples. |
| [`time.rs`](src/time.rs) | Production clock and the feature-gated deterministic E2E clock behind the same API. |
| [`guide.md`](src/guide.md) | The practical guide sent from the template settings screen. |
| [`schema.sql`](src/schema.sql) | Reference snapshot of the complete current SQLite schema, checked by a migration unit test. |

`src/main.rs` is intentionally small: it initializes tracing, installs the
interrupt handlers, constructs `App`, serves it, and returns its result.
`src/lib.rs` declares the modules, re-exports `App`, and exposes migration
support for [`examples/migrate.rs`](examples/migrate.rs).

## Database model

The database has four application tables, each with a singleton row where
appropriate:

- `inbox` is append-only input history. Mail Message-IDs and Telegram update IDs
  are deduplicated through `(source, external_id)`; mail without a Message-ID
  is always a new input.
- `tracker` stores the active flag, hike timestamps and locations, the latest
  alert body, and reminder/recovery state.
- `runtime` stores the last tick time, next deadline, current settings screen,
  and `last_processed_inbox_id`.
- `settings` stores the owner and safety reminder schedules and the two safety
  message templates.

Fresh databases and databases already managed by SeaORM are supported. Database
initialization connects and runs the SeaORM migrator; it does not reset or
silently reinterpret an unrelated SQLite file. Inbox cleanup runs once before
admission and approximately once a day, deleting rows older than six days whose
IDs are below the processed cursor.

To apply migrations without starting the server:

```sh
cargo run --locked --example migrate
```

The example accepts an optional SQLite URL as its first argument.

## Telegram and notifications

Application code calls Frankenstein’s `AsyncTelegramApi` methods directly. The
local bot wrapper supplies bounded retries—at most three attempts—for transient
connection, timeout, rate-limit, and server failures. It also honors Telegram’s
`retry_after` value. The wrapper is the transport boundary; business code does
not need a separate `telegram_call` helper.

With an active hike the owner keyboard contains `OK` and `FINISHED`. Otherwise
it contains `Start hike` and `Settings`. Owner commands and OK acknowledgements
are quiet; overdue owner reminders, safety alerts, and safety recovery use
notifications.

An empty `telegram_webhook_url` selects long polling. The first poll omits an
offset; subsequent polls use a process-local offset after successful admission.
Telegram redelivery is safe because update IDs are deduplicated in `inbox`.
With a webhook URL, `setWebhook` runs before serving and `deleteWebhook` runs
as Axum shuts down gracefully.

## Safety templates

There are exactly two configurable templates: `safety_alert` and
`safety_recovery`. They are strict and fuel-limited MiniJinja templates. A
candidate is rendered against examples before it is saved, and rendered output
is checked for a non-empty message and Telegram’s size limit. If rendering or
rich-message delivery fails for a safety notification, the application logs a
safe error and sends a static fallback message.

The template context is the tracker itself, so fields such as `started_at`,
`last_ok_at`, `last_event_at`, `started_location`, `location`, `last_alert`, and
`active` can be used directly. Bare date-time values render as Telegram relative
time widgets. The `date` and `time` filters select calendar-date and time-of-day
widgets; locations render as Telegram map components. See
[`src/guide.md`](src/guide.md) for practical examples.

## Configuration and HTTP interface

Configuration is loaded from `./config.yaml` using Figment:

```yaml
telegram_bot_token: "YOUR_TOKEN"
owner_chat_id: 123
safety_chat_id: 456
ok_regex: 'ALL OK'
finished_regex: 'FINISHED'
# Empty or omitted selects long polling:
telegram_webhook_url: ''
# Optional; defaults to https://api.telegram.org:
telegram_api_url: 'https://api.telegram.org'
# Optional; defaults to 0.0.0.0:8080:
listen_address: '0.0.0.0:8080'
```

The server exposes:

- `POST /mail` for non-empty mail payloads;
- `POST /tg` for Telegram JSON updates;
- `GET /healthz` for a no-content health response.

Requests are limited to 1 MiB. Inputs are durably inserted before the HTTP
handler returns success. Run one application instance per SQLite database and
keep the token-bearing configuration private.

## Principles

- Keep ownership explicit: `App` orchestrates infrastructure, while focused
  modules implement operations over `Ctx`.
- Persist inputs before interpreting them. The append-only inbox and runtime
  cursor make retries, crashes, duplicate deliveries, and ignored inputs
  observable and safe.
- Commit only after external Telegram work succeeds. Singleton changes from a
  tick are committed together in one short transaction; inbox admission is a
  separate transaction.
- Prefer direct data access and small operations over service, repository,
  adapter, outbox, or setter layers.
- Keep transport concerns at the transport boundary. Telegram retries and test
  mocks belong in `bot.rs`; notification intent and presentation belong in
  `notify.rs` and the owner-facing modules.
- Test observable behavior. Unit tests use an in-memory Mockall bot; E2E tests
  run the real binary, SQLite, and Frankenstein HTTP client against a local
  Telegram transcript server.
- Log outcomes with safe metadata. Tokens and message bodies never enter error
  logs.

## Releases

Ordinary pushes run checks without publishing. Use Conventional Commits:
`feat:` prepares a minor release; `fix:` and dependency maintenance prepare a
patch. **Actions → Release-plz → Run workflow** on `main` prepares or refreshes
the release PR and changelog. Merge a minor-release PR when ready. Weekly runs
can auto-merge patch-release PRs after checks pass, including ordinary fixes.
See [release setup and recovery](.github/RELEASING.md) for the one-time GitHub App,
ruleset, and baseline-tag setup.

## Verification

Open this repository with VS Code's **Dev Containers: Rebuild and Reopen in
Container** command. The `dev` target in [`Dockerfile`](Dockerfile) provides
Debian Bookworm, the latest stable Rust toolchain (including rustfmt, Clippy,
and rust-src), prek, release-plz, actionlint, GitHub CLI, and jq. It runs as the
`vscode` user and installs the Git
pre-commit hook automatically. Nix, Devenv, and direnv are no longer required.

Run every check with live tool output:

```sh
make check
```

The [Makefile](Makefile) defines formatting,
Clippy for production and all features with warnings denied, production tests,
tests with E2E enabled, workflow linting, and Git diff
whitespace checks. Every commit runs the
same checks through the [prek configuration](.pre-commit-config.yaml), which
calls `make check`. `prek run --all-files` also runs all checks,
but buffers tool output. Outside the devcontainer, install Make, Rust, prek,
and actionlint, then run
`prek install --force` once to replace any old Devenv hook.

GitHub CI builds the `checks` Docker target, which inherits `dev` and runs
`make check` directly so tool output streams into the build log.
The production image builds from `checks`, compiles the release binary without
the E2E feature, and copies it into the Debian 12 distroless runtime. To run
these builds locally from the repository root:

```sh
docker build --pull --target dev -t sat-tracker-dev .
docker build --pull --target checks .
docker build --pull -t sat-tracker .
```

Rust's `stable` channel and prek's `latest` image are resolved when their build
steps run. Use `--no-cache --pull` (or VS Code's **Rebuild Container Without
Cache**) to refresh an existing environment. CI refreshes the development
stage and executes checks on every build. Keep `.git` in the build context:
the application embeds Git commit and dirty-state metadata.

The default build is the production build. The `e2e` feature only replaces the
clock with a deterministic virtual clock; E2E still exercises the real binary
and real network client.

```sh
make rustfmt
make clippy
make clippy-all-features
make tests
make tests-e2e
make workflows
make diff-check
```

E2E transcripts live in [`tests/scenarios/`](tests/scenarios/). Every outgoing
Telegram call needs an explicit transcript response. `wait: <duration>` advances
the virtual clock; `restart`, `crash`, and `exit: failure` are used only for
persistence and failure-recovery scenarios. The production build has no `/time`
route.
