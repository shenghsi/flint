use anyhow::Result;
use serde_json::Value;

use crate::migrations::migrate_settings;

pub(crate) fn migrate_git_panel_sort_by_path(value: &mut Value) -> Result<()> {
    migrate_settings(value, &mut |settings| {
        let Some(git_panel) = settings.get_mut("git_panel").and_then(Value::as_object_mut) else {
            return Ok(());
        };
        let Some(sort_by_path) = git_panel.get("sort_by_path").and_then(Value::as_bool) else {
            return Ok(());
        };

        if !git_panel.contains_key("sort_by") {
            git_panel.insert(
                "sort_by".to_string(),
                Value::String(if sort_by_path { "path" } else { "name" }.to_string()),
            );
        }
        git_panel.remove("sort_by_path");
        Ok(())
    })
}
