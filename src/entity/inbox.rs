use crate::state::{DateTimeUtc, IngressSource};
use sea_orm::entity::prelude::*;

/// Inbox records are accepted before business processing. The migration owns
/// the partial pending index and source/external-id uniqueness constraint.
#[sea_orm::model]
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "inbox")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub source: IngressSource,
    pub external_id: Option<String>,
    pub received_at: DateTimeUtc,
    pub payload: Option<Vec<u8>>,
    pub processed_at: Option<DateTimeUtc>,
}

impl ActiveModelBehavior for ActiveModel {}
