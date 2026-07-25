use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use tokio::{sync::Notify, task::JoinHandle};
use uuid::Uuid;

use crate::{
    action::{self, Action, ActionContext, ActionId, HandlerError},
    error::{Error, RunnerError},
    storage::{QueuedInput, Storage, timestamp_ms},
};

/// Builds a database and its action registry.
pub struct Builder<S> {
    initializer: Box<dyn FnOnce() -> S + Send>,
    handlers: HashMap<&'static str, Arc<dyn ErasedHandler<S>>>,
    registration_error: Option<Error>,
}

impl<S> Builder<S>
where
    S: Serialize + DeserializeOwned + Send + 'static,
{
    /// Create a builder with the state used only when initializing a new
    /// database.
    pub fn new(initial_state: S) -> Self {
        Self::with_initializer(move || initial_state)
    }

    /// Create a builder with a lazily evaluated initializer. The initializer
    /// is called only if the database does not contain state yet.
    pub fn with_initializer<F>(initializer: F) -> Self
    where
        F: FnOnce() -> S + Send + 'static,
    {
        Self {
            initializer: Box::new(initializer),
            handlers: HashMap::new(),
            registration_error: None,
        }
    }

    /// Register an action and the runtime dependencies held by its value.
    pub fn register<A>(mut self, action: A) -> Self
    where
        A: Action<State = S>,
    {
        if let Err(error) = validate_name(A::NAME) {
            self.registration_error.get_or_insert(error);
            return self;
        }
        if self
            .handlers
            .insert(A::NAME, Arc::new(ActionHandler { action }))
            .is_some()
        {
            self.registration_error
                .get_or_insert_with(|| Error::DuplicateAction(A::NAME.to_owned()));
        }
        self
    }

    /// Open the database, recover interrupted work, and spawn its runner.
    pub async fn open(self, path: impl AsRef<Path>) -> Result<(Handle, Runner), Error> {
        if let Some(error) = self.registration_error {
            return Err(error);
        }
        let runner_guard = RunnerGuard::acquire(path.as_ref())?;
        let storage = Storage::open(path.as_ref().to_owned()).await?;
        let handlers = Arc::new(self.handlers);
        let registered = Arc::new(handlers.keys().copied().collect::<HashSet<_>>());
        let state_payload = storage
            .initialize(self.initializer, handlers.keys().copied().collect())
            .await?;
        let state: S = serde_json::from_slice(&state_payload)?;
        let shared = Arc::new(Shared {
            storage,
            notify: Notify::new(),
            shutdown: AtomicBool::new(false),
            registered,
        });
        let handle = Handle {
            shared: Arc::clone(&shared),
        };
        let task = tokio::spawn(run_loop(state, handlers, shared, runner_guard));
        Ok((handle, Runner { task }))
    }
}

/// A clonable, concurrent handle to the durable queue.
#[derive(Clone)]
pub struct Handle {
    shared: Arc<Shared>,
}

impl Handle {
    /// Durably enqueue an action, then wake the scheduler.
    pub async fn enqueue<A: Action>(&self, parameters: &A::Parameters) -> Result<ActionId, Error> {
        self.enqueue_at::<A>(parameters, SystemTime::now()).await
    }

    /// Durably enqueue an action for execution at a wall-clock time.
    pub async fn enqueue_at<A: Action>(
        &self,
        parameters: &A::Parameters,
        run_at: SystemTime,
    ) -> Result<ActionId, Error> {
        validate_name(A::NAME)?;
        validate_registered(&self.shared.registered, A::NAME)?;
        if self.shared.shutdown.load(Ordering::Acquire) {
            return Err(Error::ShuttingDown);
        }
        let id = ActionId(Uuid::new_v4());
        self.shared
            .storage
            .enqueue(QueuedInput {
                id,
                name: A::NAME.to_owned(),
                payload: serde_json::to_vec(parameters)?,
                run_at_ms: timestamp_ms(run_at),
            })
            .await?;
        self.shared.notify.notify_one();
        Ok(id)
    }

    /// Durably enqueue an action for execution after a duration.
    pub async fn enqueue_after<A: Action>(
        &self,
        parameters: &A::Parameters,
        delay: Duration,
    ) -> Result<ActionId, Error> {
        let run_at = SystemTime::now()
            .checked_add(delay)
            .ok_or(Error::ScheduleOverflow)?;
        self.enqueue_at::<A>(parameters, run_at).await
    }

    /// Durably cancel a pending action. Returns whether it was pending.
    pub async fn cancel(&self, id: ActionId) -> Result<bool, Error> {
        if self.shared.shutdown.load(Ordering::Acquire) {
            return Err(Error::ShuttingDown);
        }
        let cancelled = self.shared.storage.cancel(id).await?;
        self.shared.notify.notify_one();
        Ok(cancelled)
    }

    /// Request graceful shutdown. The current handler, if any, is allowed to
    /// finish; no new handler is selected.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.notify.notify_one();
    }
}

/// Awaitable ownership of the spawned runner.
pub struct Runner {
    task: JoinHandle<Result<(), RunnerError>>,
}

impl Future for Runner {
    type Output = Result<(), RunnerError>;

    fn poll(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match Pin::new(&mut self.task).poll(context) {
            std::task::Poll::Ready(Ok(result)) => std::task::Poll::Ready(result),
            std::task::Poll::Ready(Err(error)) => {
                std::task::Poll::Ready(Err(RunnerError::Task(error)))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

struct Shared {
    storage: Storage,
    notify: Notify,
    shutdown: AtomicBool,
    registered: Arc<HashSet<&'static str>>,
}

#[async_trait]
trait ErasedHandler<S>: Send + Sync {
    async fn run(&self, state: &mut S, payload: &[u8]) -> Result<(), HandlerError>;
}

struct ActionHandler<A> {
    action: A,
}

#[async_trait]
impl<S, A> ErasedHandler<S> for ActionHandler<A>
where
    S: Send,
    A: Action<State = S>,
{
    async fn run(&self, state: &mut S, payload: &[u8]) -> Result<(), HandlerError> {
        let parameters =
            serde_json::from_slice(payload).map_err(|error| Box::new(error) as HandlerError)?;
        self.action.run(state, parameters).await
    }
}

async fn run_loop<S>(
    mut state: S,
    handlers: Arc<HashMap<&'static str, Arc<dyn ErasedHandler<S>>>>,
    shared: Arc<Shared>,
    _runner_guard: RunnerGuard,
) -> Result<(), RunnerError>
where
    S: Serialize + DeserializeOwned + Send + 'static,
{
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return Ok(());
        }
        let notified = shared.notify.notified();
        let Some(next) = shared.storage.next_pending().await? else {
            notified.await;
            continue;
        };
        let now = timestamp_ms(SystemTime::now());
        if next.run_at_ms > now {
            let duration = Duration::from_millis((next.run_at_ms - now) as u64);
            tokio::select! {
                () = tokio::time::sleep(duration) => {}
                () = notified => continue,
            }
        }
        if shared.shutdown.load(Ordering::Acquire) {
            return Ok(());
        }
        let Some(queued) = shared.storage.claim(next.id).await? else {
            continue;
        };
        let handler = handlers
            .get(queued.name.as_str())
            .expect("registered actions were validated before runner startup");
        let context = ActionContext {
            operations: Vec::new(),
            registered: Arc::clone(&shared.registered),
        };
        let result = action::scope(context, handler.run(&mut state, &queued.payload)).await;
        let operations = match result {
            Ok(((), operations)) => operations,
            Err(source) => {
                return Err(RunnerError::Handler {
                    id: queued.id,
                    name: queued.name,
                    source,
                });
            }
        };
        let state_payload = serde_json::to_vec(&state).map_err(Error::from)?;
        shared
            .storage
            .complete(queued.id, state_payload, operations)
            .await?;
    }
}

pub(crate) fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty() {
        Err(Error::EmptyActionName)
    } else {
        Ok(())
    }
}

pub(crate) fn validate_registered(
    registered: &HashSet<&'static str>,
    name: &str,
) -> Result<(), Error> {
    if registered.contains(name) {
        Ok(())
    } else {
        Err(Error::UnknownAction(name.to_owned()))
    }
}

static ACTIVE_RUNNERS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

struct RunnerGuard {
    path: PathBuf,
}

impl RunnerGuard {
    fn acquire(path: &Path) -> Result<Self, Error> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir().map_err(Error::Path)?.join(path)
        };
        let mut active = ACTIVE_RUNNERS
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !active.insert(path.clone()) {
            return Err(Error::AlreadyRunning(path.display().to_string()));
        }
        Ok(Self { path })
    }
}

impl Drop for RunnerGuard {
    fn drop(&mut self) {
        ACTIVE_RUNNERS
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.path);
    }
}
