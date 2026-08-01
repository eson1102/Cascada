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

/// 清仓：平掉该规则产生的全部跟单持仓（并取消未成交挂单）。
#[tauri::command]
pub async fn close_rule_positions(state: S<'_>, id: String) -> Result<String, String> {
    state.close_rule_positions(&id).await
}

/// 全局熔断：一键暂停/恢复所有规则。
#[tauri::command]
pub async fn set_all_rules_enabled(state: S<'_>, enabled: bool) -> Result<usize, String> {
    let mut rules = state.rules.write();
    let mut n = 0usize;
    for r in rules.iter_mut() {
        if r.enabled != enabled {
            r.enabled = enabled;
            n += 1;
        }
    }
    drop(rules);
    state.mark_dirty();
    Ok(n)
}

#[tauri::command]
pub async fn list_trades(state: S<'_>) -> Result<Vec<Arc<Trade>>, String> {
    Ok(state.trades.read().iter().cloned().collect())
}
