use super::util::S;
use crate::core::model::*;
use std::sync::Arc;

#[tauri::command]
pub async fn list_rules(state: S<'_>) -> Result<Vec<CopyRule>, String> {
    Ok(state.rules.read().clone())
}

#[tauri::command]
pub async fn upsert_rule(state: S<'_>, rule: CopyRule) -> Result<CopyRule, String> {
    {
        let mut rules = state.rules.write();
        if let Some(existing) = rules.iter_mut().find(|r| r.id == rule.id) {
            *existing = rule.clone();
        } else {
            rules.push(rule.clone());
        }
    }
    state.mark_dirty();
    Ok(rule)
}

#[tauri::command]
pub async fn delete_rule(state: S<'_>, id: String) -> Result<(), String> {
    state.rules.write().retain(|r| r.id != id);
    state.mark_dirty();
    Ok(())
}

/// 补单：让信号端重新上报持仓，缺失的跟单订单会被补开（忽略跟单时效）。
#[tauri::command]
pub async fn resync_rule(state: S<'_>, id: String) -> Result<String, String> {
    state.resync_rule(&id).await
}

#[tauri::command]
pub async fn list_trades(state: S<'_>) -> Result<Vec<Arc<Trade>>, String> {
    Ok(state.trades.read().iter().cloned().collect())
}
