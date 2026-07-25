mod common;

use std::sync::Arc;

use common::{State, database, stop};
use durable_actions::{Action, Builder, Error, HandlerError, async_trait};
use rusqlite::Connection;
use tokio::sync::{Mutex, Notify, mpsc};

struct Noop;

#[async_trait]
impl Action for Noop {
    const NAME: &'static str = "noop";
    type State = State;
    type Parameters = ();

    async fn run(&self, _state: &mut State, (): ()) -> Result<(), HandlerError> {
        Ok(())
    }
}

struct Unknown;

#[async_trait]
impl Action for Unknown {
    const NAME: &'static str = "unknown";
    type State = State;
    type Parameters = ();

    async fn run(&self, _state: &mut State, (): ()) -> Result<(), HandlerError> {
        Ok(())
    }
}

struct ScopeProbe {
    sent: mpsc::UnboundedSender<bool>,
}

#[async_trait]
impl Action for ScopeProbe {
    const NAME: &'static str = "scope-probe";
    type State = State;
    type Parameters = ();

    async fn run(&self, _state: &mut State, (): ()) -> Result<(), HandlerError> {
        let outside = tokio::spawn(async { Noop::enqueue(&()) }).await.unwrap();
        self.sent
            .send(matches!(outside, Err(Error::NoActionContext)))
            .unwrap();
        Ok(())
    }
}

struct Block {
    started: Arc<Notify>,
    release: Arc<Notify>,
    observed: Arc<Mutex<Vec<u32>>>,
}

#[async_trait]
impl Action for Block {
    const NAME: &'static str = "block";
    type State = State;
    type Parameters = u32;

    async fn run(&self, state: &mut State, value: u32) -> Result<(), HandlerError> {
        self.started.notify_one();
        self.release.notified().await;
        state.values.push(value);
        self.observed.lock().await.push(value);
        Ok(())
    }
}

#[tokio::test]
async fn validates_registration_and_task_local_scope() {
    let (_directory, duplicate_path) = database();
    let result = Builder::new(State::default())
        .register(Noop)
        .register(Noop)
        .open(&duplicate_path)
        .await;
    assert!(matches!(result, Err(Error::DuplicateAction(name)) if name == "noop"));
    assert!(matches!(Noop::enqueue(&()), Err(Error::NoActionContext)));

    let (_directory, path) = database();
    let (sent, mut received) = mpsc::unbounded_channel();
    let (handle, runner) = Builder::new(State::default())
        .register(Noop)
        .register(ScopeProbe { sent })
        .open(&path)
        .await
        .unwrap();
    assert!(matches!(
        handle.enqueue::<Unknown>(&()).await,
        Err(Error::UnknownAction(name)) if name == "unknown"
    ));
    handle.enqueue::<ScopeProbe>(&()).await.unwrap();
    assert_eq!(received.recv().await, Some(true));
    stop(&handle, runner).await;
}

#[tokio::test]
async fn initializer_uses_migration_version_and_is_lazy() {
    let (_directory, path) = database();
    let (handle, runner) = Builder::new(State::default()).open(&path).await.unwrap();
    stop(&handle, runner).await;
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        1
    );
    drop(connection);
    let (handle, runner) = Builder::with_initializer(|| -> State {
        panic!("initializer must not run for existing state")
    })
    .open(&path)
    .await
    .unwrap();
    stop(&handle, runner).await;
}

#[tokio::test]
async fn graceful_shutdown_finishes_active_without_starting_next() {
    let (_directory, path) = database();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let (handle, runner) = Builder::new(State::default())
        .register(Block {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
            observed: Arc::clone(&observed),
        })
        .open(&path)
        .await
        .unwrap();
    handle.enqueue::<Block>(&1).await.unwrap();
    handle.enqueue::<Block>(&2).await.unwrap();
    started.notified().await;
    handle.shutdown();
    release.notify_one();
    runner.await.unwrap();
    assert_eq!(*observed.lock().await, vec![1]);
}
