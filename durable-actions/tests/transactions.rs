mod common;

use std::{io, sync::Arc, time::Duration};

use common::{State, database, stop};
use durable_actions::{Action, ActionId, Builder, HandlerError, RunnerError, async_trait};
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::{Notify, mpsc};

struct FollowUp;

#[async_trait]
impl Action for FollowUp {
    const NAME: &'static str = "follow-up";
    type State = State;
    type Parameters = u32;

    async fn run(&self, _state: &mut State, _value: u32) -> Result<(), HandlerError> {
        panic!("cancelled follow-up ran");
    }
}

struct Schedule;

#[async_trait]
impl Action for Schedule {
    const NAME: &'static str = "schedule";
    type State = State;
    type Parameters = u32;

    async fn run(&self, state: &mut State, value: u32) -> Result<(), HandlerError> {
        state.values.push(value);
        state.scheduled = Some(FollowUp::enqueue_after(&20, Duration::from_secs(60))?);
        Ok(())
    }
}

struct CancelStored;

#[async_trait]
impl Action for CancelStored {
    const NAME: &'static str = "cancel-stored";
    type State = State;
    type Parameters = ();

    async fn run(&self, state: &mut State, (): ()) -> Result<(), HandlerError> {
        Self::cancel(state.scheduled.take().unwrap())?;
        Ok(())
    }
}

struct Verify {
    sent: mpsc::UnboundedSender<(Vec<u32>, Option<ActionId>)>,
}

#[async_trait]
impl Action for Verify {
    const NAME: &'static str = "verify";
    type State = State;
    type Parameters = ();

    async fn run(&self, state: &mut State, (): ()) -> Result<(), HandlerError> {
        self.sent
            .send((state.values.clone(), state.scheduled))
            .unwrap();
        Ok(())
    }
}

struct Fail;

#[async_trait]
impl Action for Fail {
    const NAME: &'static str = "recoverable";
    type State = State;
    type Parameters = u32;

    async fn run(&self, state: &mut State, value: u32) -> Result<(), HandlerError> {
        state.values.push(value);
        Err(Box::new(io::Error::other("expected failure")))
    }
}

struct Recover {
    sent: mpsc::UnboundedSender<Vec<u32>>,
}

#[async_trait]
impl Action for Recover {
    const NAME: &'static str = "recoverable";
    type State = State;
    type Parameters = u32;

    async fn run(&self, state: &mut State, value: u32) -> Result<(), HandlerError> {
        self.sent.send(state.values.clone()).unwrap();
        state.values.push(value);
        Ok(())
    }
}

struct CommitFailure {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Action for CommitFailure {
    const NAME: &'static str = "commit-failure";
    type State = State;
    type Parameters = u32;

    async fn run(&self, state: &mut State, value: u32) -> Result<(), HandlerError> {
        state.values.push(value);
        FollowUp::enqueue(&99)?;
        self.started.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn staged_id_can_be_stored_and_cancelled_transactionally() {
    let (_directory, path) = database();
    let (sent, mut received) = mpsc::unbounded_channel();
    let (handle, runner) = Builder::new(State::default())
        .register(Schedule)
        .register(CancelStored)
        .register(Verify { sent })
        .register(FollowUp)
        .open(&path)
        .await
        .unwrap();
    handle.enqueue::<Schedule>(&1).await.unwrap();
    handle.enqueue::<CancelStored>(&()).await.unwrap();
    handle.enqueue::<Verify>(&()).await.unwrap();
    assert_eq!(received.recv().await.unwrap(), (vec![1], None));
    let connection = Connection::open(&path).unwrap();
    let count: u32 = connection
        .query_row(
            "SELECT count(*) FROM actions WHERE name = 'follow-up'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    drop(connection);
    stop(&handle, runner).await;
}

#[tokio::test]
async fn handler_failure_discards_state_and_retries_after_reopen() {
    let (_directory, path) = database();
    let (handle, runner) = Builder::new(State::default())
        .register(Fail)
        .open(&path)
        .await
        .unwrap();
    handle.enqueue::<Fail>(&7).await.unwrap();
    assert!(matches!(
        runner.await,
        Err(RunnerError::Handler { name, .. }) if name == "recoverable"
    ));
    let (sent, mut received) = mpsc::unbounded_channel();
    let (handle, runner) = Builder::new(State {
        values: vec![999],
        scheduled: None,
    })
    .register(Recover { sent })
    .open(&path)
    .await
    .unwrap();
    assert_eq!(received.recv().await.unwrap(), Vec::<u32>::new());
    stop(&handle, runner).await;
}

#[tokio::test]
async fn commit_failure_is_atomic() {
    let (_directory, path) = database();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (handle, runner) = Builder::new(State::default())
        .register(CommitFailure {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        })
        .register(FollowUp)
        .open(&path)
        .await
        .unwrap();
    handle.enqueue::<CommitFailure>(&1).await.unwrap();
    started.notified().await;
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_state_update
             BEFORE UPDATE ON state
             BEGIN SELECT RAISE(FAIL, 'injected commit failure'); END;",
        )
        .unwrap();
    release.notify_one();
    assert!(matches!(runner.await, Err(RunnerError::Engine(_))));
    let state_payload: Vec<u8> = connection
        .query_row("SELECT payload FROM state", [], |row| row.get(0))
        .unwrap();
    let state: State = serde_json::from_slice(&state_payload).unwrap();
    assert!(state.values.is_empty());
    let follow_up: Option<Vec<u8>> = connection
        .query_row(
            "SELECT id FROM actions WHERE name = 'follow-up'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert!(follow_up.is_none());
}
