use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};
use serde::Serialize;

use crate::{action::ActionId, error::Error};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS state (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        payload BLOB NOT NULL
    );
    CREATE TABLE IF NOT EXISTS actions (
        sequence INTEGER PRIMARY KEY AUTOINCREMENT,
        id BLOB NOT NULL UNIQUE,
        name TEXT NOT NULL,
        payload BLOB NOT NULL,
        run_at_ms INTEGER NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('pending', 'running'))
    );
    CREATE INDEX IF NOT EXISTS actions_next
        ON actions(status, run_at_ms, sequence);
";

#[derive(Debug)]
pub(crate) enum StagedOperation {
    Enqueue(QueuedInput),
    Cancel(ActionId),
}

#[derive(Debug)]
pub(crate) struct QueuedInput {
    pub(crate) id: ActionId,
    pub(crate) name: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) run_at_ms: i64,
}

#[derive(Debug)]
pub(crate) struct Pending {
    pub(crate) id: ActionId,
    pub(crate) run_at_ms: i64,
}

#[derive(Debug)]
pub(crate) struct Queued {
    pub(crate) id: ActionId,
    pub(crate) name: String,
    pub(crate) payload: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct Storage {
    connection: Arc<Mutex<Connection>>,
}

impl Storage {
    pub(crate) async fn open(path: PathBuf) -> Result<Self, Error> {
        let connection = tokio::task::spawn_blocking(move || open_connection(&path)).await??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    async fn call<T, F>(&self, operation: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, rusqlite::Error> + Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        Ok(tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            operation(&mut connection)
        })
        .await??)
    }

    pub(crate) async fn initialize<S>(
        &self,
        initializer: Box<dyn FnOnce() -> S + Send>,
        registered: Vec<&'static str>,
    ) -> Result<Vec<u8>, Error>
    where
        S: Serialize + Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, Error> {
            let mut connection = connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Migrations::new(vec![M::up(INITIAL_SCHEMA)]).to_latest(&mut connection)?;
            let transaction = connection.transaction()?;
            let has_state = transaction
                .query_row("SELECT 1 FROM state WHERE singleton = 1", [], |_| Ok(()))
                .optional()?
                .is_some();
            if !has_state {
                let initial_state = serde_json::to_vec(&initializer())?;
                transaction.execute(
                    "INSERT INTO state(singleton, payload) VALUES (1, ?1)",
                    [&initial_state],
                )?;
            }
            transaction.execute(
                "UPDATE actions SET status = 'pending' WHERE status = 'running'",
                [],
            )?;
            let unknown = {
                let mut statement = transaction.prepare("SELECT DISTINCT name FROM actions")?;
                let names = statement.query_map([], |row| row.get::<_, String>(0))?;
                names
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .find(|name| !registered.iter().any(|known| known == name))
            };
            if let Some(name) = unknown {
                return Err(Error::UnknownAction(name));
            }
            let state = transaction.query_row(
                "SELECT payload FROM state WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            transaction.commit()?;
            Ok(state)
        })
        .await?
    }

    pub(crate) async fn enqueue(&self, input: QueuedInput) -> Result<(), Error> {
        self.call(move |connection| {
            insert_action(connection, input)?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn cancel(&self, id: ActionId) -> Result<bool, Error> {
        self.call(move |connection| {
            let count = connection.execute(
                "DELETE FROM actions WHERE status = 'pending' AND id = ?1",
                [id.0],
            )?;
            Ok(count != 0)
        })
        .await
    }

    pub(crate) async fn next_pending(&self) -> Result<Option<Pending>, Error> {
        self.call(|connection| {
            connection
                .query_row(
                    "SELECT id, run_at_ms FROM actions
                     WHERE status = 'pending'
                     ORDER BY run_at_ms, sequence LIMIT 1",
                    [],
                    |row| {
                        Ok(Pending {
                            id: ActionId(row.get(0)?),
                            run_at_ms: row.get(1)?,
                        })
                    },
                )
                .optional()
        })
        .await
    }

    pub(crate) async fn claim(&self, id: ActionId) -> Result<Option<Queued>, Error> {
        self.call(move |connection| {
            let transaction = connection.transaction()?;
            let changed = transaction.execute(
                "UPDATE actions SET status = 'running'
                 WHERE id = ?1 AND status = 'pending'",
                [id.0],
            )?;
            if changed == 0 {
                transaction.commit()?;
                return Ok(None);
            }
            let queued = transaction.query_row(
                "SELECT id, name, payload FROM actions WHERE id = ?1",
                [id.0],
                |row| {
                    Ok(Queued {
                        id: ActionId(row.get(0)?),
                        name: row.get(1)?,
                        payload: row.get(2)?,
                    })
                },
            )?;
            transaction.commit()?;
            Ok(Some(queued))
        })
        .await
    }

    pub(crate) async fn complete(
        &self,
        id: ActionId,
        state: Vec<u8>,
        operations: Vec<StagedOperation>,
    ) -> Result<(), Error> {
        self.call(move |connection| {
            let transaction = connection.transaction()?;
            transaction.execute(
                "UPDATE state SET payload = ?1 WHERE singleton = 1",
                [&state],
            )?;
            for operation in operations {
                match operation {
                    StagedOperation::Enqueue(input) => insert_action(&transaction, input)?,
                    StagedOperation::Cancel(id) => {
                        transaction.execute(
                            "DELETE FROM actions WHERE status = 'pending' AND id = ?1",
                            [id.0],
                        )?;
                    }
                }
            }
            transaction.execute("DELETE FROM actions WHERE id = ?1", [id.0])?;
            transaction.commit()
        })
        .await
    }
}

fn open_connection(path: &Path) -> Result<Connection, rusqlite::Error> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

fn insert_action(connection: &Connection, input: QueuedInput) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO actions(id, name, payload, run_at_ms, status)
         VALUES (?1, ?2, ?3, ?4, 'pending')",
        params![input.id.0, input.name, input.payload, input.run_at_ms],
    )?;
    Ok(())
}

pub(crate) fn timestamp_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}
