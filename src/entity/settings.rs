use crate::state::ReminderMinutes;
use sea_orm::entity::prelude::*;

/// Confirmed bot settings. The migration enforces the singleton row.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "settings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    pub owner_reminder_minutes: ReminderMinutes,
    pub safety_reminder_minutes: ReminderMinutes,
}

impl ActiveModelBehavior for ActiveModel {}
