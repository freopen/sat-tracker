CREATE TABLE inbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL CHECK (source IN ('mail', 'telegram')),
    external_id TEXT,
    received_at DATETIME NOT NULL,
    payload BLOB,
    processed_at DATETIME,
    CONSTRAINT inbox_source_external_id UNIQUE (source, external_id),
    CONSTRAINT inbox_telegram_id CHECK (
        source != 'telegram' OR (external_id IS NOT NULL AND length(external_id) > 0)
    )
);

CREATE INDEX inbox_pending ON inbox (id) WHERE processed_at IS NULL;

CREATE TABLE runtime (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_tick_at DATETIME NOT NULL DEFAULT '1970-01-01 00:00:00',
    next_tick_at DATETIME,
    settings_position TEXT NOT NULL DEFAULT 'main' CHECK (settings_position IN ('main', 'settings', 'owner_reminder_times', 'safety_reminder_times', 'safety_alert_template', 'safety_recovery_template')),
    last_processed_inbox_id BIGINT NOT NULL DEFAULT 0 CHECK (last_processed_inbox_id >= 0)
);

CREATE TABLE "seaql_migrations" (
    "version" varchar NOT NULL PRIMARY KEY,
    "applied_at" integer NOT NULL
);

CREATE TABLE settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    owner_reminder_minutes JSON NOT NULL,
    safety_reminder_minutes JSON NOT NULL,
    safety_alert_template TEXT NOT NULL DEFAULT '',
    safety_recovery_template TEXT NOT NULL DEFAULT ''
);

CREATE TABLE tracker (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    active BOOLEAN NOT NULL DEFAULT FALSE CHECK (active IN (0, 1)),
    started_at DATETIME,
    started_location JSON,
    last_event_at DATETIME,
    last_ok_at DATETIME,
    last_alert TEXT,
    location JSON,
    finished_at DATETIME NOT NULL DEFAULT '1970-01-01 00:00:00',
    owner_reminders_sent INTEGER NOT NULL DEFAULT 0 CHECK (owner_reminders_sent >= 0),
    safety_reminders_sent INTEGER NOT NULL DEFAULT 0 CHECK (safety_reminders_sent >= 0),
    safety_alerted BOOLEAN NOT NULL DEFAULT FALSE CHECK (safety_alerted IN (0, 1)),
    CONSTRAINT tracker_active_state CHECK (
        (active = FALSE AND started_at IS NULL AND last_ok_at IS NULL
            AND last_event_at IS NULL AND finished_at = '1970-01-01 00:00:00'
            AND last_alert IS NULL AND started_location IS NULL AND location IS NULL
            AND owner_reminders_sent = 0 AND safety_reminders_sent = 0
            AND safety_alerted = FALSE)
        OR (active = TRUE AND started_at IS NOT NULL
            AND last_ok_at IS NOT NULL AND last_event_at IS NOT NULL
            AND started_at <= last_ok_at AND last_ok_at <= last_event_at)
        OR (active = FALSE AND started_at IS NOT NULL
            AND last_ok_at IS NOT NULL AND last_event_at IS NOT NULL
            AND started_at <= last_ok_at AND last_ok_at <= finished_at
            AND finished_at <= last_event_at)
    ),
    CONSTRAINT tracker_reminder_alerts CHECK (
        safety_reminders_sent = 0 OR safety_alerted = TRUE
    )
);
