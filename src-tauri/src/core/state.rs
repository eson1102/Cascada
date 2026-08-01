use crate::connectors::file_bridge::cascada_root;
use crate::connectors::{spawn_connector, ConnectorHandle};
use crate::core::engine::{translate_symbol, CopyEngine};
use crate::core::events::{LogEntry, LogLevel, EVT_ACCOUNT, EVT_LOG, EVT_QUOTE, EVT_SYMBOLS, EVT_TRADE};
use crate::core::model::*;
use crate::core::persistence::{self, Snapshot};
use crate::core::ticket_map::{MasterKey, TicketMap};
use crate::sidecar::TvProxyManager;
use anyhow::Result;
use dashmap::DashMap;
use chrono::{Datelike, Timelike};
use parking_lot::RwLock;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Notify};

const TRADE_BUFFER_CAP: usize = 1000;
const SAVE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);
/// Minimum gap between front-end quote events for the same (account, symbol).
/// Tick streams can fire >50 Hz; this caps UI refresh at 5 Hz per pair.
const QUOTE_EMIT_THROTTLE: std::time::Duration = std::time::Duration::from_millis(200);

fn next_log_id() -> u64 {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

pub struct AppState {
    pub accounts: DashMap<String, Account>,
    pub rules: RwLock<Vec<CopyRule>>,
    pub trades: RwLock<VecDeque<Arc<Trade>>>,
    pub connectors: DashMap<String, ConnectorHandle>,
    pub ticket_map: Arc<TicketMap>,
    /// Latest quote per (account_id, uppercased symbol). Updated on every tick;
    /// front-end emission is throttled separately via `quote_last_emit`.
    pub quotes: DashMap<(String, String), Quote>,
    quote_last_emit: DashMap<(String, String), std::time::Instant>,
    /// Tracks per-rule auto-close actions (e.g. weekend sweep) so the
    /// watchdog doesn't fire repeatedly within the same calendar day.
    pub last_close: DashMap<String, String>,
    /// Per-account heartbeat-timeout alert latch (true = already alerted).
    pub hb_alerted: DashMap<String, bool>,
    /// Per-source throttle for failure desktop notifications (ms timestamps).
    pub last_fail_notify: DashMap<String, i64>,
    /// Per-symbol throttle for quote-driven trailing-stop checks.
    pub trailing_check_at: DashMap<String, std::time::Instant>,
    /// Rules that already ran the post-restart position reconciliation
    /// (slave Resync → master Resync, rebuilding ticket_map so pre-existing
    /// positions keep following master closes).
    pub synced_rules: DashMap<String, bool>,
    /// Active per-account subscription set (uppercased symbols). Authoritative
    /// source replayed to the EA on reconnect.
    pub subscriptions: DashMap<String, Vec<String>>,
    /// Latest broker watchlist per account (uppercased, refreshed on demand).
    pub symbols: DashMap<String, Vec<String>>,
    pub event_tx: mpsc::UnboundedSender<ConnectorEvent>,
    event_rx: parking_lot::Mutex<Option<mpsc::UnboundedReceiver<ConnectorEvent>>>,
    /// Cascada-managed Python sidecar that bridges TradingView browser
    /// sessions onto the file-bridge protocol. Constructed once at app
    /// startup; commands access it via `state.tv_proxy.{setup,start,stop}`.
    pub tv_proxy: Arc<TvProxyManager>,
    app_handle: OnceLock<AppHandle>,
    save_dirty: AtomicBool,
    save_notify: Notify,
}

impl AppState {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let tv_proxy = Arc::new(TvProxyManager::new(tx.clone()));
        Self {
            accounts: DashMap::new(),
            rules: RwLock::new(Vec::new()),
            trades: RwLock::new(VecDeque::with_capacity(TRADE_BUFFER_CAP)),
            connectors: DashMap::new(),
            quotes: DashMap::new(),
            quote_last_emit: DashMap::new(),
            last_close: DashMap::new(),
            hb_alerted: DashMap::new(),
            last_fail_notify: DashMap::new(),
            trailing_check_at: DashMap::new(),
            synced_rules: DashMap::new(),
            subscriptions: DashMap::new(),
            symbols: DashMap::new(),
            ticket_map: Arc::new(TicketMap::new()),
            event_tx: tx,
            event_rx: parking_lot::Mutex::new(Some(rx)),
            tv_proxy,
            app_handle: OnceLock::new(),
            save_dirty: AtomicBool::new(false),
            save_notify: Notify::new(),
        }
    }

    pub fn attach_app_handle(&self, h: AppHandle) {
        // Hand the bundled python-build-standalone distribution path to the
        // TV proxy manager. Tauri exposes it via the `tauri::Manager` →
        // `path().resource_dir()` route; the python tarball is extracted
        // under `<resource_dir>/python/` by the release CI (see release.yml).
        // Missing in dev builds and on non-bundled paths — TvProxyManager
        // silently falls back to PATH detection in that case.
        use tauri::Manager;
        if let Ok(rdir) = h.path().resource_dir() {
            self.tv_proxy.set_bundled_python_dir(rdir.join("python"));
        }
        let _ = self.app_handle.set(h);
    }

    /// 系统桌面通知（tauri-plugin-notification）；未初始化/无权限时静默忽略。
    pub fn notify(&self, title: &str, body: &str) {
        use tauri_plugin_notification::NotificationExt;
        if let Some(h) = self.app_handle.get() {
            let _ = h.notification()
                .builder()
                .title(title.to_string())
                .body(body.to_string())
                .show();
        }
    }

    pub fn emit_log(&self, level: LogLevel, source: &str, message: impl Into<String>) {
        let msg = message.into();
        match level {
            LogLevel::Error => tracing::error!("[{source}] {msg}"),
            LogLevel::Warn => tracing::warn!("[{source}] {msg}"),
            LogLevel::Info => tracing::info!("[{source}] {msg}"),
        }
        // 下单/平仓失败 → 桌面通知（节流：每 30 秒至多一条，避免刷屏）
        if matches!(level, LogLevel::Error)
            && (msg.contains("failed") || msg.contains("failed ") || msg.contains("失败"))
        {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let last = self.last_fail_notify.get(source).map(|v| *v.value()).unwrap_or(0);
            if now_ms - last > 30_000 {
                self.last_fail_notify.insert(source.to_string(), now_ms);
                self.notify("跟单操作失败", &format!("[{source}] {msg}"));
            }
        }
        if let Some(h) = self.app_handle.get() {
            let _ = h.emit(EVT_LOG, LogEntry {
                id: next_log_id(),
                ts: chrono::Utc::now().timestamp_millis(),
                level, source: source.into(), message: msg,
            });
        }
    }

    fn emit_account(&self, a: &Account) {
        if let Some(h) = self.app_handle.get() { let _ = h.emit(EVT_ACCOUNT, a); }
    }

    /// Same as `emit_account` but callable from outside the module —
    /// commands that mutate accounts directly (rename, set_role) need this
    /// so the UI receives the change without waiting for a manual refresh.
    pub fn emit_account_public(&self, a: &Account) { self.emit_account(a); }

    fn emit_trade(&self, t: &Trade) {
        if let Some(h) = self.app_handle.get() { let _ = h.emit(EVT_TRADE, t); }
    }

    /// Throttled quote emission: drops events when the last emit for this
    /// `key` was less than `QUOTE_EMIT_THROTTLE` ago. The latest value is
    /// always retained in `quotes`, so `list_quotes` returns fresh data.
    /// Takes the pre-built key by reference so the hot path doesn't clone it
    /// twice (once for `quotes`, once for throttle map).
    fn emit_quote_throttled(&self, key: &(String, String), q: &Quote) {
        let now = std::time::Instant::now();
        let should_emit = match self.quote_last_emit.get(key) {
            Some(prev) => now.duration_since(*prev) >= QUOTE_EMIT_THROTTLE,
            None => true,
        };
        if !should_emit { return; }
        self.quote_last_emit.insert(key.clone(), now);
        if let Some(h) = self.app_handle.get() { let _ = h.emit(EVT_QUOTE, q); }
    }

    /// Mark the snapshot dirty; the debounce loop flushes it to disk.
    pub fn mark_dirty(&self) {
        self.save_dirty.store(true, Ordering::Relaxed);
        self.save_notify.notify_one();
    }

    pub fn spawn_save_loop(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.save_notify.notified().await;
                tokio::time::sleep(SAVE_DEBOUNCE).await;
                if this.save_dirty.swap(false, Ordering::Relaxed) {
                    if let Err(e) = this.save_to_disk().await {
                        tracing::warn!("save failed: {e}");
                    }
                }
            }
        });
    }

    /// Apply `f` to the account under `id`, emit the update only if `f` returned true.
    fn with_account(&self, id: &str, f: impl FnOnce(&mut Account) -> bool) {
        if let Some(mut a) = self.accounts.get_mut(id) {
            if f(&mut a) { self.emit_account(&a); }
        }
    }

    pub async fn load_from_disk(self: &Arc<Self>) -> Result<()> {
        let snap = persistence::load().await?;
        for mut a in snap.accounts {
            // Connectors haven't dialled in yet — start every account offline
            // and let the first heartbeat flip the pill back to online.
            a.connected = false;
            a.balance = 0.0;
            a.equity = 0.0;
            self.accounts.insert(a.id.clone(), a);
        }
        *self.rules.write() = snap.rules;
        Ok(())
    }

    pub async fn save_to_disk(&self) -> Result<()> {
        // Serialize under the rules read-lock so we never clone the account
        // map or rules vector — the custom `SnapshotRef` borrows straight
        // from `self`. Drop the guard before the async write.
        let bytes = {
            let rules = self.rules.read();
            let snap = persistence::SnapshotRef {
                accounts: &self.accounts,
                rules: rules.as_slice(),
            };
            serde_json::to_vec_pretty(&snap)?
        };
        persistence::save_bytes(bytes).await
    }

    /// Cloned snapshot — used by the user-facing export command, not the
    /// hot debounced autosave path (that one borrows via `SnapshotRef`).
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            accounts: self.accounts.iter().map(|kv| kv.value().clone()).collect(),
            rules: self.rules.read().clone(),
        }
    }

    /// Replace all accounts + rules with the supplied snapshot.
    /// Disconnects every active connector first; caller should call
    /// `reconnect_all` afterwards to wire up the imported accounts.
    pub async fn replace_with(self: &Arc<Self>, snap: Snapshot) {
        let ids: Vec<String> = self.connectors.iter().map(|kv| kv.key().clone()).collect();
        for id in ids { let _ = self.disconnect(&id).await; }
        self.accounts.clear();
        for mut a in snap.accounts {
            a.connected = false;
            a.balance = 0.0;
            a.equity = 0.0;
            self.accounts.insert(a.id.clone(), a.clone());
            self.emit_account(&a);
        }
        *self.rules.write() = snap.rules;
        self.trades.write().clear();
        self.ticket_map.clear();
        self.quotes.clear();
        self.quote_last_emit.clear();
        self.subscriptions.clear();
        self.symbols.clear();
        self.mark_dirty();
    }

    pub async fn start_engine(self: &Arc<Self>) {
        let rx = self.event_rx.lock().take().expect("engine already started");
        let this = self.clone();
        let engine = Arc::new(CopyEngine::new(this.clone()));
        tokio::spawn(async move {
            let mut rx = rx;
            while let Some(ev) = rx.recv().await {
                this.handle_event(ev, engine.as_ref()).await;
            }
        });
    }

    async fn handle_event(&self, ev: ConnectorEvent, engine: &CopyEngine) {
        match ev {
            ConnectorEvent::Connected { account_id, login, balance, equity, currency } => {
                let mut should_save = false;
                self.with_account(&account_id, |a| {
                    let changed = !a.connected || a.balance != balance
                        || a.equity != equity || a.currency != currency;
                    a.connected = true;
                    a.balance = balance;
                    a.equity = equity;
                    a.currency = currency;
                    if !login.is_empty() && a.login != login {
                        a.login = login;
                        should_save = true;
                    }
                    changed || should_save
                });
                if should_save { self.mark_dirty(); }
                self.emit_log(LogLevel::Info, &account_id, "connected");
            }
            ConnectorEvent::Disconnected { account_id } => {
                self.with_account(&account_id, |a| {
                    if !a.connected { return false; }
                    a.connected = false;
                    true
                });
                self.emit_log(LogLevel::Warn, &account_id, "disconnected");
            }
            ConnectorEvent::Heartbeat { account_id, balance, equity } => {
                let now_ms = chrono::Utc::now().timestamp_millis();
                self.with_account(&account_id, |a| {
                    let changed = !a.connected || a.balance != balance || a.equity != equity
                        || a.last_seen != now_ms;
                    a.connected = true;
                    a.balance = balance;
                    a.equity = equity;
                    a.last_seen = now_ms;
                    changed
                });
            }
            ConnectorEvent::TradeOpened(mut t) => {
                // Tag the record with its rule (statistics grouping). Try the
                // existing map first (already-confirmed mirrors), then the
                // pending entry consumed by resolve_slave_open below.
                let mut rule_id = self.ticket_map.rule_for_slave(&t.account_id, &t.ticket);
                let is_mirror = t.origin_ticket.as_deref().map_or(false, |origin| {
                    let matched = self.ticket_map.resolve_slave_open(
                        &t.account_id, origin, &t.ticket,
                    );
                    if matched {
                        self.emit_log(LogLevel::Info, &t.account_id,
                            format!("mirror {} ↔ master {origin}", t.ticket));
                    }
                    matched
                });
                if is_mirror && rule_id.is_empty() {
                    rule_id = self.ticket_map.rule_for_slave(&t.account_id, &t.ticket);
                }
                // 重启/升级后：slave 持仓无 origin（旧订单无 c: 备注且无持久化记录）
                // 时，尝试按 (翻译后品种, 方向, 手数) 匹配同规则 master 持仓回填映射。
                if rule_id.is_empty() && !is_mirror && t.origin_ticket.is_none() {
                    if let Some(filled) = self.backfill_slave_from_master(&t) {
                        t.rule_id = filled.clone();
                        rule_id = filled;
                        self.emit_log(LogLevel::Info, &t.account_id,
                            format!("回填映射：slave {} ↔ master（品种/方向/手数匹配）", t.ticket));
                    }
                }
                if !rule_id.is_empty() { t.rule_id = rule_id; }
                // 登记移动止损/保本跟踪（仅 mirror 的 slave 持仓）
                if is_mirror {
                    engine.track_slave(&t.ticket, &t.account_id, &t.rule_id,
                        &t.symbol, t.side, t.price,
                        t.sl.unwrap_or(0.0), t.tp.unwrap_or(0.0), t.pip_size);
                }
                let t = Arc::new(t);
                {
                    let mut trades = self.trades.write();
                    trades.push_front(Arc::clone(&t));
                    if trades.len() > TRADE_BUFFER_CAP { trades.pop_back(); }
                }
                self.emit_trade(&t);
                // A master TradeOpened whose ticket already has slave mappings
                // is the position born from a pending we already mirrored
                // (ticket_map was migrated by PendingFilled). Skip the engine
                // fan-out so the slave pending can fill on its own instead of
                // receiving a duplicate market order.
                let already_mapped = !is_mirror && self.ticket_map.has_master(
                    &crate::core::ticket_map::MasterKey {
                        account_id: t.account_id.clone(),
                        ticket: t.ticket.clone(),
                    });
                if !is_mirror && !already_mapped {
                    engine.on_trade_opened(&t).await;
                }
            }
            ConnectorEvent::TradeClosed { ticket, account_id, profit, ts } => {
                // Build a minimal Trade payload only for frontend emit; engine
                // only needs (account_id, ticket) and skips the Trade struct.
                let t = Trade {
                    ticket: ticket.clone(), account_id: account_id.clone(),
                    symbol: String::new(), side: Side::Buy, volume: 0.0, price: 0.0,
                    sl: None, tp: None,
                    opened_at: ts, closed_at: Some(ts), profit,
                    origin_ticket: None, comment: String::new(), pip_size: 0.0, unrealized: None,
                    feed: String::new(), magic: 0, resync: false,
                    rule_id: String::new(),
                };
                self.emit_trade(&t);
                engine.on_trade_closed(&account_id, &ticket).await;
            }
            ConnectorEvent::HistoricalTrade(t) => {
                let t = Arc::new(t);
                {
                    let mut trades = self.trades.write();
                    trades.push_back(Arc::clone(&t));
                    if trades.len() > TRADE_BUFFER_CAP { trades.pop_front(); }
                }
                self.emit_trade(&t);
            }
            ConnectorEvent::TradeModified(t) => {
                self.emit_trade(&t);
                engine.on_trade_modified(&t).await;
            }
            ConnectorEvent::Log { account_id, level, message } => {
                self.emit_log(level, &account_id, message);
            }
            ConnectorEvent::Quote(mut q) => {
                // ASCII-uppercase in place — tickers are always ASCII, so this
                // avoids a full UTF-8 `to_uppercase()` allocation per tick.
                q.symbol.make_ascii_uppercase();
                let key = (q.account_id.clone(), q.symbol.clone());
                self.emit_quote_throttled(&key, &q);
                // 移动止损/保本：quote 驱动（1 秒节流，避免高频 tick 触发）
                let now = std::time::Instant::now();
                let last = self.trailing_check_at.get(&q.symbol)
                    .map(|v| *v.value()).unwrap_or(std::time::Instant::now() - std::time::Duration::from_secs(10));
                if now.duration_since(last) >= std::time::Duration::from_secs(1) {
                    self.trailing_check_at.insert(q.symbol.clone(), now);
                    engine.trailing_check(&q.symbol, q.bid, q.ask).await;
                }
                self.quotes.insert(key, q);
            }
            ConnectorEvent::PendingOpened(p) => {
                // Mirror-detection: if a slave EA reports a pending whose
                // `origin_ticket` matches one we dispatched, wire the slave
                // ticket into the ticket_map and skip re-dispatching to
                // avoid infinite mirror loops.
                let is_mirror = p.origin_ticket.as_deref().map_or(false, |origin| {
                    let matched = self.ticket_map.resolve_slave_open(
                        &p.account_id, origin, &p.ticket,
                    );
                    if matched {
                        self.emit_log(LogLevel::Info, &p.account_id,
                            format!("pending mirror {} ↔ master {origin}", p.ticket));
                    }
                    matched
                });
                if !is_mirror { engine.on_pending_opened(&p).await; }
            }
            ConnectorEvent::PendingModified(p) => {
                engine.on_pending_modified(&p).await;
            }
            ConnectorEvent::PendingCancelled { account_id, ticket } => {
                engine.on_pending_cancelled(&account_id, &ticket).await;
            }
            ConnectorEvent::PendingFilled { ticket, account_id, position_ticket } => {
                // Slave pending fills on its own when its broker reaches the
                // target — we don't re-dispatch. But on cTrader the resulting
                // position has a new ID, so migrate the master↔slave mapping
                // onto that ID. The migration also lets the master-side
                // TradeOpened handler recognize "this position is from an
                // already-mirrored pending" and skip the duplicate dispatch.
                if let Some(pid) = position_ticket.as_deref() {
                    self.ticket_map.migrate_ticket(&account_id, &ticket, pid);
                }
                self.emit_log(LogLevel::Info, &account_id,
                    format!("pending {ticket} filled"));
            }
            ConnectorEvent::Symbols { account_id, symbols } => {
                // Preserve the broker's original case — some brokers expose
                // suffixed symbols like "US500.cash" where "US500.CASH" would
                // fail a `MarketInfo` / `SymbolInfoDouble` lookup. We dedupe
                // case-insensitively (stable order) so "EURUSD" vs "eurusd"
                // don't both show up.
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                let mut canon: Vec<String> = Vec::with_capacity(symbols.len());
                for s in symbols {
                    let trimmed = s.trim();
                    if trimmed.is_empty() { continue; }
                    let upper = trimmed.to_uppercase();
                    if seen.insert(upper) { canon.push(trimmed.to_string()); }
                }
                canon.sort_by(|a, b| a.to_ascii_uppercase().cmp(&b.to_ascii_uppercase()));
                self.symbols.insert(account_id.clone(), canon.clone());
                if let Some(h) = self.app_handle.get() {
                    let _ = h.emit(EVT_SYMBOLS, (&account_id, &canon));
                }
            }
        }
    }

    /// Send a list-symbols request to the EA. The reply arrives asynchronously
    /// via `ConnectorEvent::Symbols` and updates `symbols`.
    pub async fn request_symbols(&self, id: &str) -> bool {
        if let Some(h) = self.connector_handle(id) {
            let _ = h.send(ConnectorCmd::ListSymbols).await;
            true
        } else { false }
    }

    /// Replace this account's symbol subscription set and push it to the EA.
    /// Preserves the caller's case so case-sensitive broker tickers
    /// (e.g. `US500.cash`) reach the EA intact. Dedupe is case-insensitive.
    pub async fn set_subscription(&self, id: &str, symbols: Vec<String>) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut canon: Vec<String> = Vec::with_capacity(symbols.len());
        for s in symbols {
            let trimmed = s.trim();
            if trimmed.is_empty() { continue; }
            let upper = trimmed.to_uppercase();
            if seen.insert(upper) { canon.push(trimmed.to_string()); }
        }
        canon.sort_by(|a, b| a.to_ascii_uppercase().cmp(&b.to_ascii_uppercase()));
        self.subscriptions.insert(id.to_string(), canon.clone());
        // Drop any cached quotes for symbols we no longer subscribe to.
        // Quotes are stored with uppercased symbols (see `Quote` arm above),
        // so the keep-set is uppercase to match.
        let keep: std::collections::HashSet<String> = canon.iter().map(|s| s.to_uppercase()).collect();
        self.quotes.retain(|(acc, sym), _| acc != id || keep.contains(sym.as_str()));
        self.quote_last_emit.retain(|(acc, sym), _| acc != id || keep.contains(sym.as_str()));
        if let Some(h) = self.connector_handle(id) {
            let _ = h.send(ConnectorCmd::Subscribe { symbols: canon.clone() }).await;
        }
        canon
    }

    pub async fn connect(self: &Arc<Self>, id: &str) -> Result<()> {
        let account = self.accounts.get(id).map(|a| a.clone())
            .ok_or_else(|| anyhow::anyhow!("unknown account"))?;
        // MT4/MT5/TradingView attach automatically via the file-discovery loop.
        if matches!(account.platform,
            Platform::MT4 | Platform::MT5 | Platform::TradingView) { return Ok(()); }
        if self.connectors.contains_key(id) { return Ok(()); }
        let handle = spawn_connector(account, self.event_tx.clone())?;
        self.connectors.insert(id.to_string(), handle.clone());
        // Replay any persisted subscription so the EA resumes streaming.
        if let Some(syms) = self.subscriptions.get(id) {
            let symbols = syms.clone();
            if !symbols.is_empty() {
                let _ = handle.send(ConnectorCmd::Subscribe { symbols }).await;
            }
        }
        Ok(())
    }

    /// 补单：请信号端 EA 重新上报全部当前持仓（open 事件带 resync 标记）。
    /// 引擎对已跟单的持仓自动跳过，缺失的持仓会补开（绕过交易时效过滤）。
    pub async fn resync_rule(self: &Arc<Self>, rule_id: &str) -> Result<String, String> {
        let rule = self.rules.read().iter()
            .find(|r| r.id == rule_id).cloned()
            .ok_or_else(|| "找不到该规则".to_string())?;
        let master_id = rule.master_id.clone();
        let handle = self.connectors.get(&master_id)
            .ok_or_else(|| "信号端账户未连接".to_string())?;
        handle.send(crate::core::model::ConnectorCmd::Resync).await
            .map_err(|e| e.to_string())?;
        Ok(format!("已请求补单（信号端 {} 的持仓将重新同步）", master_id))
    }

    /// 清仓：平掉该规则产生的全部跟单持仓（并取消未成交挂单）。
    pub async fn close_rule_positions(self: &Arc<Self>, rule_id: &str) -> Result<String, String> {
        let rule = self.rules.read().iter()
            .find(|r| r.id == rule_id).cloned()
            .ok_or_else(|| "找不到该规则".to_string())?;
        let mut closed = 0usize;
        for s in self.ticket_map.slaves_for_rule(rule_id) {
            if let Some(h) = self.connectors.get(&s.account_id) {
                if h.send(ConnectorCmd::Close { ticket: s.ticket.clone() }).await.is_ok() {
                    closed += 1;
                }
            }
        }
        let mut cancelled = 0usize;
        for (acc, origin) in self.ticket_map.pendings_for_rule(rule_id) {
            if let Some(h) = self.connectors.get(&acc) {
                if h.send(ConnectorCmd::CancelPending { ticket: origin.clone() }).await.is_ok() {
                    cancelled += 1;
                }
            }
        }
        Ok(format!("已发出平仓 {} 笔、取消挂单 {} 笔（规则：{}）",
            closed, cancelled, rule.name))
    }

    /// Background sweep — every 5s. Enforces per-rule `max_floating_loss`
    /// (closes the rule's slave positions when unrealised loss crosses the
    /// USD threshold) and `weekend_close` (Friday 20:00 UTC sweep).
    pub fn start_watchdog(self: &Arc<Self>) {
        let s = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                s.tick_risk().await;
            }
        });
        // 独立循环：自动订阅 + 重启对账（避免在 tick_risk 里持有非 Send 引用）
        let s2 = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                s2.ensure_rule_subscriptions().await;
                s2.reconcile_rule_mappings().await;
            }
        });
    }

    async fn tick_risk(self: &Arc<Self>) {
        let rules = self.rules.read().clone();
        for rule in rules {
            if !rule.enabled { continue; }
            // ---- 浮亏监控 ----
            if rule.max_floating_loss > 0.0 {
                if let Some(loss) = self.rule_floating_loss(&rule) {
                    if loss >= rule.max_floating_loss {
                        self.emit_log(LogLevel::Warn, &rule.master_id,
                            format!("浮亏 ${:.2} 超过阈值 ${:.2}，自动清仓（规则：{}）",
                                loss, rule.max_floating_loss, rule.name));
                        self.notify(&format!("浮亏自动清仓（{}）", rule.name),
                            &format!("浮亏 ${:.2} 超过阈值 ${:.2}，已全部平仓。",
                                loss, rule.max_floating_loss));
                        let _ = self.close_rule_positions(&rule.id).await;
                    }
                }
            }
            // ---- 周末清盘：周五 20:00 UTC（5 分钟窗口）----
            if rule.weekend_close {
                let now = chrono::Utc::now();
                if now.weekday() == chrono::Weekday::Fri
                    && now.hour() == 20 && now.minute() < 5
                {
                    let today = now.format("%Y-%m-%d").to_string();
                    let dup = self.last_close.get(&rule.id).map(|v| v.value().clone());
                    if dup.as_deref() != Some(&today) {
                        self.emit_log(LogLevel::Info, &rule.master_id,
                            format!("周末清盘（周五 20:00 UTC）触发（规则：{}）", rule.name));
                        self.notify(&format!("周末清盘（{}）", rule.name),
                            "周五 20:00 UTC 到达，该规则全部跟单持仓已平仓，避开周末跳空。");
                        let _ = self.close_rule_positions(&rule.id).await;
                        self.last_close.insert(rule.id.clone(), today);
                    }
                }
            }
        }

        // ---- 信号端心跳超时告警（30 秒无心跳）----
        let now_ms = chrono::Utc::now().timestamp_millis();
        for kv in self.accounts.iter() {
            let id = kv.key().clone();
            let (role, last_seen, label) = (kv.value().role, kv.value().last_seen, kv.value().label.clone());
            if role != crate::core::model::AccountRole::Master || last_seen <= 0 { continue; }
            let age_ms = now_ms - last_seen;
            let alerted = self.hb_alerted.get(&id).map(|v| *v.value()).unwrap_or(false);
            if age_ms > 30_000 && !alerted {
                self.hb_alerted.insert(id.clone(), true);
                self.emit_log(LogLevel::Warn, &id,
                    format!("信号端「{}」心跳超时（{} 秒无数据），可能已断开！",
                        label, age_ms / 1000));
                self.notify(&format!("信号端「{label}」心跳超时"),
                    &format!("已 {} 秒没有收到信号端数据，请检查 MT4/MT5 终端与 EA 是否在线。", age_ms / 1000));
            } else if age_ms <= 30_000 && alerted {
                self.hb_alerted.insert(id.clone(), false);
            }
        }
    }

    /// 估算该规则下全部 slave 持仓的浮亏（账户货币）。
    /// 优先用 EA 上报的 quote.unrealized（该品种全部持仓的浮动盈亏，
    /// 100 就是 100，无需合约估算）；新版 EA 未上报时回退到
    /// 差价 × 合约大小启发式（旧 EA 的近似）。
    /// 直接扫 trades（按 rule_id + 未平仓），不依赖 ticket_map ——
    /// 这样即使映射尚未回填（重启初期）也能监控。
    fn rule_floating_loss(&self, rule: &CopyRule) -> Option<f64> {
        let trades = self.trades.read();
        let mut total = 0.0_f64;
        // 收集 (account, symbol) 去重集合 —— EA 报的是符号级浮亏
        let mut seen = std::collections::HashSet::new();
        for t in trades.iter() {
            if t.rule_id != rule.id || t.closed_at.is_some() { continue; }
            if !seen.insert((t.account_id.clone(), t.symbol.clone())) { continue; }
            let q = self.quotes.get(&(t.account_id.clone(), t.symbol.clone())).map(|v| v.value().clone());
            let Some(q) = q else { continue; };
            // 优先：EA 上报的符号级浮亏（账户货币，直接相加）
            if let Some(u) = q.unrealized {
                if u < 0.0 { total += -u; }
                continue;
            }
            // 回退：差价 × 合约大小启发式
            let units = (t.volume * contract_size_for(&t.symbol)).max(0.0);
            let is_buy = matches!(t.side, Side::Buy);
            let current = if is_buy { q.bid } else { q.ask };
            let loss = if is_buy {
                (t.price - current) * units
            } else {
                (current - t.price) * units
            };
            if loss > 0.0 { total += loss; }
        }
        Some(total)
    }

    /// 重启/升级后回填：slave 持仓无 origin 时，按 (翻译后品种, 方向, 手数)
    /// 匹配同规则 master 的未映射持仓，直接建立映射。多义时放弃（宁缺毋滥）。
    fn backfill_slave_from_master(&self, t: &Trade) -> Option<String> {
        let rules = self.rules.read().clone();
        let trades = self.trades.read();
        for rule in rules {
            if !rule.enabled || rule.slave_id != t.account_id { continue; }
            let mut match_cnt = 0usize;
            let mut matched: Option<(String, String)> = None;
            for m in trades.iter() {
                if m.account_id != rule.master_id || m.closed_at.is_some() { continue; }
                if translate_symbol(&rule, &m.symbol) != t.symbol || m.side != t.side { continue; }
                let vol_diff = (m.volume - t.volume).abs();
                if vol_diff > m.volume.max(t.volume) * 0.01 + 0.001 { continue; }
                // master 已映射过（该持仓已有 slave 跟随）则不参与匹配
                if !self.ticket_map.slaves_for(&MasterKey {
                    account_id: m.account_id.clone(), ticket: m.ticket.clone(),
                }).is_empty() { continue; }
                match_cnt += 1;
                matched = Some((m.account_id.clone(), m.ticket.clone()));
                if match_cnt > 1 { return None; }
            }
            if let Some((ma, mt)) = matched {
                self.ticket_map.backfill(&t.account_id, &t.ticket,
                    MasterKey { account_id: ma, ticket: mt }, rule.id.clone());
                return Some(rule.id.clone());
            }
        }
        None
    }

    /// 确保启用规则涉及的 slave 账户订阅其持仓品种——浮亏监控与
    /// 报价驱动功能都依赖 EA 上报 quote。仅在订阅集合变化时下发命令。
    pub async fn ensure_rule_subscriptions(self: &Arc<Self>) {
        // 用块作用域把 trades 读锁限制在收集阶段，避免 guard 跨 await 存活
        let needed: std::collections::HashMap<String, std::collections::BTreeSet<String>> = {
            let rules = self.rules.read().clone();
            let trades = self.trades.read();
            let mut needed: std::collections::HashMap<String, std::collections::BTreeSet<String>> = Default::default();
            for rule in rules {
                if !rule.enabled { continue; }
                // slave 端已持仓的品种（直接按 slave 符号订阅）
                for t in trades.iter() {
                    if t.account_id == rule.slave_id && t.closed_at.is_none() {
                        needed.entry(rule.slave_id.clone())
                            .or_default()
                            .insert(t.symbol.clone());
                    }
                }
            }
            needed
        };
        for (acc, syms) in needed {
            let list: Vec<String> = syms.into_iter().collect();
            let cur = self.subscriptions.get(&acc).map(|v| v.value().clone()).unwrap_or_default();
            if cur == list { continue; }
            self.set_subscription(&acc, list).await;
        }
    }

    /// 重启后的持仓对账：每个启用规则在 master/slave 双方都连接后，
    /// 先让 slave 上报持仓（回填映射），2 秒后再让 master 上报持仓
    /// （引擎幂等匹配 orphan slave，跳过重复下单）。这样 app/EA 重启后
    /// 旧持仓的 master↔slave 映射得以重建，平仓能继续跟随。
    pub async fn reconcile_rule_mappings(self: &Arc<Self>) {
        let rules = self.rules.read().clone();
        for rule in rules {
            if !rule.enabled { continue; }
            if self.synced_rules.contains_key(&rule.id) { continue; }
            let master_conn = self.accounts.get(&rule.master_id).map(|a| a.connected).unwrap_or(false);
            let slave_conn = self.accounts.get(&rule.slave_id).map(|a| a.connected).unwrap_or(false);
            if !master_conn || !slave_conn { continue; }
            // 先 slave 后 master，保证 master resync 时能找到 orphan slave
            if let Some(h) = self.connector_handle(&rule.slave_id) {
                let _ = h.send(ConnectorCmd::Resync).await;
            }
            let s = self.clone();
            let master_id = rule.master_id.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if let Some(h) = s.connector_handle(&master_id) {
                    let _ = h.send(ConnectorCmd::Resync).await;
                }
            });
            self.synced_rules.insert(rule.id.clone(), true);
            self.emit_log(LogLevel::Info, &rule.master_id,
                format!("重启对账：已请求同步规则「{}」的持仓映射", rule.name));
        }
    }

    /// Look up an MT account by (platform, login) or create one on the fly.
    /// Used by the MT multiplexer when an EA dials in with an unknown login.
    pub async fn find_or_create_mt_account(
        self: &Arc<Self>, platform: Platform, login: &str, server: &str,
    ) -> Account {
        let existing_id = self.accounts.iter()
            .find(|kv| kv.value().platform == platform && kv.value().login == login)
            .map(|kv| kv.key().clone());
        if let Some(id) = existing_id {
            let mut a = self.accounts.get_mut(&id).unwrap();
            if !server.is_empty() && a.server != server {
                a.server = server.to_string();
                let snapshot = a.clone();
                drop(a);
                self.mark_dirty();
                return snapshot;
            }
            return a.clone();
        }
        let pname = match platform { Platform::MT4 => "MT4", Platform::MT5 => "MT5", _ => "MT" };
        let account = Account {
            id: uuid::Uuid::new_v4().to_string(),
            platform,
            label: format!("{pname} {login}"),
            login: login.to_string(),
            server: server.to_string(),
            role: AccountRole::Idle,
            connected: false,
            balance: 0.0, equity: 0.0,
            currency: "USD".into(),
        last_seen: 0,
            password: None,
        };
        self.accounts.insert(account.id.clone(), account.clone());
        self.mark_dirty();
        if let Some(h) = self.app_handle.get() { let _ = h.emit(EVT_ACCOUNT, &account); }
        self.emit_log(LogLevel::Info, &account.id,
            format!("auto-discovered {pname} account {login}"));
        account
    }

    pub fn spawn_mt_discovery(self: &Arc<Self>) {
        crate::connectors::mt_bridge::spawn_discovery(self.clone());
    }

    /// Look up a TradingView account by login (the proxy-chosen identifier,
    /// typically the TV `account_id`) or create one on the fly. Always
    /// auto-roled as Master — TV is read-only in v1.
    pub async fn find_or_create_tv_account(self: &Arc<Self>, login: &str) -> Account {
        // Single map pass: clone the matching value directly instead of
        // collecting the id and re-looking it up (which races with removal
        // and panics on .unwrap()).
        let existing = self.accounts.iter()
            .find(|kv| kv.value().platform == Platform::TradingView && kv.value().login == login)
            .map(|kv| kv.value().clone());
        if let Some(a) = existing { return a; }
        let account = Account {
            id: uuid::Uuid::new_v4().to_string(),
            platform: Platform::TradingView,
            label: format!("TradingView {login}"),
            login: login.to_string(),
            server: String::new(),
            role: AccountRole::Master,
            connected: false,
            balance: 0.0, equity: 0.0,
            currency: "USD".into(),
        last_seen: 0,
            password: None,
        };
        self.accounts.insert(account.id.clone(), account.clone());
        self.mark_dirty();
        if let Some(h) = self.app_handle.get() { let _ = h.emit(EVT_ACCOUNT, &account); }
        self.emit_log(LogLevel::Info, &account.id,
            format!("auto-discovered TradingView account {login}"));
        account
    }

    pub fn spawn_tv_discovery(self: &Arc<Self>) {
        crate::connectors::tv_bridge::spawn_discovery(self.clone());
    }

    /// If a TradingView account is already in the saved snapshot, kick off
    /// the bundled sidecar in the background so the user doesn't have to
    /// click "Start" on every Cascada launch. Errors land in the log
    /// stream — failure shouldn't block app startup. No-op when no TV
    /// account exists.
    pub fn spawn_tv_proxy_autostart(self: &Arc<Self>) {
        let has_tv = self.accounts.iter()
            .any(|kv| kv.value().platform == Platform::TradingView);
        if !has_tv { return; }
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(e) = this.tv_proxy.start().await {
                this.emit_log(LogLevel::Warn, "tv-proxy",
                    format!("auto-start failed: {e} — open the TradingView \
                             tab in Accounts and click Setup."));
            }
        });
    }

    pub async fn disconnect(&self, id: &str) -> Result<()> {
        if let Some((_, h)) = self.connectors.remove(id) {
            h.shutdown().await;
        }
        self.with_account(id, |a| { a.connected = false; true });
        Ok(())
    }

    pub fn connector_handle(&self, id: &str) -> Option<ConnectorHandle> {
        self.connectors.get(id).map(|h| h.clone())
    }

    pub fn reconnect_all(self: &Arc<Self>) {
        let ids: Vec<String> = self.accounts.iter().map(|kv| kv.key().clone()).collect();
        let this = self.clone();
        tokio::spawn(async move {
            for id in ids {
                if let Err(e) = this.connect(&id).await {
                    this.emit_log(LogLevel::Warn, &id, format!("auto-connect failed: {e}"));
                }
            }
        });
    }

    pub fn spawn_ctrader_discovery(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                tick.tick().await;
                let Some(root) = cascada_root() else { continue };
                let Ok(mut rd) = tokio::fs::read_dir(&root).await else { continue };
                while let Ok(Some(entry)) = rd.next_entry().await {
                    let p = entry.path();
                    if !p.is_dir() { continue; }
                    let Some(login) = p.file_name().and_then(|s| s.to_str()) else { continue };
                    if login.is_empty() { continue; }
                    // The TradingView sidecar writes its sessions under the
                    // dedicated subfolder; skip it so we don't try to register
                    // it as a cTrader login.
                    if login == crate::connectors::tv_bridge::TV_SUBDIR { continue; }
                    if !tokio::fs::try_exists(p.join("events.jsonl")).await.unwrap_or(false) { continue; }
                    let already = this.accounts.iter().any(|kv| {
                        kv.value().platform == Platform::CTrader && kv.value().login == login
                    });
                    if already { continue; }
                    let account = Account {
                        id: uuid::Uuid::new_v4().to_string(),
                        platform: Platform::CTrader,
                        label: format!("cTrader {login}"),
                        login: login.to_string(),
                        server: String::new(),
                        role: AccountRole::Idle,
                        connected: false,
                        balance: 0.0, equity: 0.0,
                        currency: "USD".into(),
                        last_seen: 0,
                        password: None,
                    };
                    let id = account.id.clone();
                    this.accounts.insert(id.clone(), account.clone());
                    this.mark_dirty();
                    if let Some(h) = this.app_handle.get() {
                        let _ = h.emit(EVT_ACCOUNT, &account);
                    }
                    this.emit_log(LogLevel::Info, &id,
                        format!("auto-discovered cTrader account {login}"));
                    let _ = this.connect(&id).await;
                }
            }
        });
    }
}

/// 按品种符号推断 1 标手的合约大小（报价货币单位）。
/// 外汇直盘/交叉盘按 100000；黄金 XAU 100 盎司、白银 XAG 5000 盎司；
/// 加密货币与指数按 1（直接以价格波动计损益）。用于浮亏估算。
fn contract_size_for(symbol: &str) -> f64 {
    let s = symbol.to_ascii_uppercase();
    if s.contains("XAU") || s.contains("GOLD") { return 100.0; }
    if s.contains("XAG") || s.contains("SILVER") { return 5000.0; }
    if s.contains("BTC") || s.contains("ETH") || s.contains("XRP")
        || s.contains("LTC") || s.contains("BCH") || s.contains("DOGE")
        || s.contains("SOL") || s.contains("ADA") || s.contains("USDT")
        || s.contains("USD1") || s.contains("CRYPTO") { return 1.0; }
    if s.contains("US30") || s.contains("NAS") || s.contains("SPX")
        || s.contains("US500") || s.contains("USTEC") || s.contains("UK100")
        || s.contains("GER30") || s.contains("DAX") || s.contains("JP225")
        || s.contains("NIKKEI") || s.contains("AUS200") || s.contains("ESP35")
        || s.contains("FRA40") || s.contains("EUSTX") || s.contains("HK50")
        || s.contains("CH50") || s.contains("VIX") { return 1.0; }
    100_000.0
}
