use crate::state::DateTimeUtc;
use sea_orm::entity::prelude::*;

/// Process-wide durable scheduling state. The migration enforces id = 1.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "runtime")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    pub telegram_poll_offset: i64,
    pub last_tick_at: Option<DateTimeUtc>,
    pub next_tick_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
