use std::sync::Arc;

use anyhow::Result;
use gpui::{App, Context, SharedString, Task, Window};

use crate::Workspace;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredPickerState {
    pub query: String,
    pub multi_select_enabled: bool,
    pub selected_item_ids: Vec<SharedString>,
}

pub trait ReopenablePickerRequest: 'static {
    fn is_valid(&self, workspace: &Workspace, cx: &App) -> bool;

    fn reopen(
        &self,
        state: StoredPickerState,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<()>>;
}

pub struct PickerHistoryEntry {
    pub request: Arc<dyn ReopenablePickerRequest>,
    pub state: StoredPickerState,
}

impl Clone for PickerHistoryEntry {
    fn clone(&self) -> Self {
        Self {
            request: self.request.clone(),
            state: self.state.clone(),
        }
    }
}
