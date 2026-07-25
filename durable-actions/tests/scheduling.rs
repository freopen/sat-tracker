mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use common::{State, database, stop};
use durable_actions::{Action, Builder, HandlerError, async_trait};
use tokio::sync::{Mutex, Notify, mpsc};

struct Report {
    sent: mpsc::UnboundedSender<(u32, Vec<u32>)>,
}

#[async_trait]
impl Action for Report {
    const NAME: &'static str = "report";
    type State = State;
    type Parameters = u32;

    async fn run(&self, state: &mut State, value: u32) -> Result<(), HandlerError> {
        state.values.push(value);
        self.sent.send((value, state.values.clone())).unwrap();
        Ok(())
    }
}

struct Serial {
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
    finished: Arc<Notify>,
}

#[async_trait]
impl Action for Serial {
    const NAME: &'static str = "serial";
    type State = State;
    type Parameters = u32;

    async fn run(&self, state: &mut State, value: u32) -> Result<(), HandlerError> {
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        state.values.push(value);
        self.active.fetch_sub(1, Ordering::SeqCst);
        if state.values.len() == 8 {
            self.finished.notify_one();
        }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initializes_reopens_and_preserves_equal_time_fifo_order() {
    let (_directory, path) = database();
    let (sent, mut received) = mpsc::unbounded_channel();
    let (handle, runner) = Builder::new(State::default())
        .register(Report { sent })
        .open(&path)
        .await
        .unwrap();
    let at = SystemTime::now() + Duration::from_millis(50);
    for value in [1, 2, 3] {
        handle.enqueue_at::<Report>(&value, at).await.unwrap();
    }
    for expected in [1, 2, 3] {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), received.recv())
                .await
                .unwrap()
                .unwrap()
                .0,
            expected
        );
    }
    stop(&handle, runner).await;

    let (sent, mut received) = mpsc::unbounded_channel();
    let (handle, runner) = Builder::new(State {
        values: vec![999],
        scheduled: None,
    })
    .register(Report { sent })
    .open(&path)
    .await
    .unwrap();
    handle.enqueue::<Report>(&4).await.unwrap();
    assert_eq!(received.recv().await.unwrap().1, vec![1, 2, 3, 4]);
    stop(&handle, runner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_action_runs_at_a_time() {
    let (_directory, path) = database();
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(Notify::new());
    let (handle, runner) = Builder::new(State::default())
        .register(Serial {
            active: Arc::clone(&active),
            maximum: Arc::clone(&maximum),
            finished: Arc::clone(&finished),
        })
        .open(&path)
        .await
        .unwrap();
    let mut enqueues = Vec::new();
    for value in 0..8 {
        let handle = handle.clone();
        enqueues.push(tokio::spawn(async move {
            handle.enqueue::<Serial>(&value).await.unwrap();
        }));
    }
    for enqueue in enqueues {
        enqueue.await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), finished.notified())
        .await
        .unwrap();
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
    stop(&handle, runner).await;
}

#[tokio::test]
async fn enqueue_and_cancel_work_while_an_action_is_blocked() {
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
    let running = handle.enqueue::<Block>(&1).await.unwrap();
    started.notified().await;
    let pending = handle.enqueue::<Block>(&2).await.unwrap();
    assert!(!handle.cancel(running).await.unwrap());
    assert!(handle.cancel(pending).await.unwrap());
    release.notify_one();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(*observed.lock().await, vec![1]);
    stop(&handle, runner).await;
}
