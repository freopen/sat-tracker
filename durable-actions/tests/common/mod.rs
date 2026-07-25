use std::path::PathBuf;

use durable_actions::ActionId;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

#[derive(Debug, Default, Deserialize, Serialize)]
pub(crate) struct State {
    pub(crate) values: Vec<u32>,
    pub(crate) scheduled: Option<ActionId>,
}

pub(crate) fn database() -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("actions.sqlite");
    (directory, path)
}

pub(crate) async fn stop(handle: &durable_actions::Handle, runner: durable_actions::Runner) {
    handle.shutdown();
    runner.await.unwrap();
}
