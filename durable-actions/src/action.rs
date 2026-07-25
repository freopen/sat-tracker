use std::{
    cell::RefCell,
    error::Error as StdError,
    fmt,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{
    error::Error,
    storage::{QueuedInput, StagedOperation, timestamp_ms},
};

/// An error returned by an application action.
pub type HandlerError = Box<dyn StdError + Send + Sync + 'static>;

/// Stable public identity of a queued action.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct ActionId(pub(crate) Uuid);

impl fmt::Display for ActionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A durable action kind and its runtime dependencies.
#[async_trait]
pub trait Action: Send + Sync + 'static {
    /// Stable name persisted with queued instances.
    const NAME: &'static str;

    /// State exclusively owned by the action runner.
    type State: Serialize + DeserializeOwned + Send + 'static;

    /// Persisted parameters for one queued action instance.
    type Parameters: Serialize + DeserializeOwned + Send + 'static;

    /// Execute one action instance.
    async fn run(
        &self,
        state: &mut Self::State,
        parameters: Self::Parameters,
    ) -> Result<(), HandlerError>;

    /// Transactionally stage an action for immediate execution.
    fn enqueue(parameters: &Self::Parameters) -> Result<ActionId, Error>
    where
        Self: Sized,
    {
        with_action_context(|context| context.enqueue_at::<Self>(parameters, SystemTime::now()))
    }

    /// Transactionally stage an action for execution at a wall-clock time.
    fn enqueue_at(parameters: &Self::Parameters, run_at: SystemTime) -> Result<ActionId, Error>
    where
        Self: Sized,
    {
        with_action_context(|context| context.enqueue_at::<Self>(parameters, run_at))
    }

    /// Transactionally stage an action for execution after a duration.
    fn enqueue_after(parameters: &Self::Parameters, delay: Duration) -> Result<ActionId, Error>
    where
        Self: Sized,
    {
        let run_at = SystemTime::now()
            .checked_add(delay)
            .ok_or(Error::ScheduleOverflow)?;
        Self::enqueue_at(parameters, run_at)
    }

    /// Transactionally stage cancellation of a pending action.
    fn cancel(id: ActionId) -> Result<(), Error>
    where
        Self: Sized,
    {
        with_action_context(|context| {
            context.operations.push(StagedOperation::Cancel(id));
            Ok(())
        })
    }
}

tokio::task_local! {
    static ACTION_CONTEXT: RefCell<Option<ActionContext>>;
}

#[derive(Debug)]
pub(crate) struct ActionContext {
    pub(crate) operations: Vec<StagedOperation>,
    pub(crate) registered: std::sync::Arc<std::collections::HashSet<&'static str>>,
}

impl ActionContext {
    fn enqueue_at<A: Action>(
        &mut self,
        parameters: &A::Parameters,
        run_at: SystemTime,
    ) -> Result<ActionId, Error> {
        crate::engine::validate_name(A::NAME)?;
        crate::engine::validate_registered(&self.registered, A::NAME)?;
        let id = ActionId(Uuid::new_v4());
        self.operations.push(StagedOperation::Enqueue(QueuedInput {
            id,
            name: A::NAME.to_owned(),
            payload: serde_json::to_vec(parameters)?,
            run_at_ms: timestamp_ms(run_at),
        }));
        Ok(id)
    }
}

fn with_action_context<T>(
    operation: impl FnOnce(&mut ActionContext) -> Result<T, Error>,
) -> Result<T, Error> {
    ACTION_CONTEXT
        .try_with(|context| {
            let mut context = context
                .try_borrow_mut()
                .map_err(|_| Error::ActionContextBusy)?;
            operation(context.as_mut().ok_or(Error::NoActionContext)?)
        })
        .map_err(|_| Error::NoActionContext)?
}

pub(crate) async fn scope<T>(
    context: ActionContext,
    future: impl std::future::Future<Output = Result<T, HandlerError>>,
) -> Result<(T, Vec<StagedOperation>), HandlerError> {
    ACTION_CONTEXT
        .scope(RefCell::new(Some(context)), async {
            let value = future.await?;
            let operations = ACTION_CONTEXT.with(|context| {
                context
                    .borrow_mut()
                    .take()
                    .expect("action context disappeared during handler execution")
                    .operations
            });
            Ok((value, operations))
        })
        .await
}
