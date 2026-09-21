pub mod categories;
pub mod db;
pub mod query;
pub mod ratio;
pub mod views;
pub mod workflows;

pub(crate) const MAX_AUTOMATION_STATE_JSON_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("persisted automation state exceeds the configured byte limit")]
pub struct AutomationStateCapacityError;

pub use categories::Category;
pub(crate) use categories::CategoryTagCapacityError;
pub use db::{AppEventRow, Db, TorrentRow};
pub use query::{
    bounded_page_limit, validate_page_offset, ListParams, TorrentLiveRow, MAX_API_PAGE_ENTRIES,
    MAX_API_PAGE_OFFSET,
};
pub use ratio::RatioGroup;
pub use views::{SavedView, SavedViewParams};
pub use workflows::{
    QbitRssItemCapacityError, RssRule, RssRuleMatch, RssRuleRenameResult, RssRuleUpsertResult,
    WorkflowRule, WorkflowRun,
};

pub(crate) fn is_category_tag_capacity_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<CategoryTagCapacityError>().is_some())
}

pub(crate) fn is_qbit_rss_item_capacity_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<QbitRssItemCapacityError>().is_some())
}

pub(crate) fn is_automation_state_capacity_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<AutomationStateCapacityError>()
            .is_some()
    })
}
