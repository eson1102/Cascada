import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Platform = "cTrader" | "MT4" | "MT5" | "TradingView";
export type AccountRole = "Master" | "Slave" | "Idle";

export interface Account {
  id: string;
  platform: Platform;
  label: string;
  login: string;
  server: string;
  role: AccountRole;
  connected: boolean;
  balance: number;
  equity: number;
  currency: string;
  /** 最近一次心跳的毫秒时间戳；0 = 从未收到。 */
  last_seen?: number;
}

export type LotMode = "Fixed" | "Multiplier" | "Equity" | "BalanceRatio" | "RiskPercent";
export type DirectionFilter = "All" | "BuyOnly" | "SellOnly";
export type SlTpMode = "Copy" | "Ignore" | "Fixed";

export interface Schedule {
  enabled: boolean;
  start_min: number;   // minutes since midnight
  end_min: number;
  skip_weekends: boolean;
}

export interface CopyRule {
  id: string;
  name: string;
  master_id: string;
  slave_id: string;
  enabled: boolean;
  lot_mode: LotMode;
  lot_value: number;
  reverse: boolean;
  max_slippage_pips: number;
  /** 规则下单魔术号（跟单端订单使用此 Magic；0 = EA 默认）。 */
  slave_magic: number;
  symbol_map: Record<string, string>;

  min_lot: number;
  max_lot: number;

  /** Signal-side (master) lot filters — skip trades whose original master
   *  volume falls outside the window (0 = bound off). Distinct from
   *  min_lot/max_lot which clamp the *slave* volume after sizing. */
  master_min_lot: number;
  master_max_lot: number;

  /** Signal-side (master) magic-number filter (MT4/MT5 only). Trade is
   *  skipped unless its magic appears in this list; empty = filter off. */
  master_magic_list: number[];
  /** 不跟随挂单：信号端的挂单（限价/止损单）不镜像到跟单端。 */
  ignore_pending: boolean;

  /** Custom comment written onto slave orders. `order_comment_src_lot`
   *  appends a `[SRC <lots>]` marker with the master's original volume. */
  order_comment: string;
  order_comment_src_lot: boolean;

  symbol_whitelist: string[];
  symbol_blacklist: string[];
  symbol_prefix: string;
  symbol_suffix: string;
  master_strip_prefix: string;
  master_strip_suffix: string;

  direction: DirectionFilter;
  comment_filter: string;
  close_on_master_close: boolean;

  max_open_positions: number;
  max_exposure_lots: number;
  max_daily_loss: number;
  /** 自动清仓阈值：浮亏超过此值（USD）时 watchdog 全部平仓。0 = 关闭。 */
  max_floating_loss: number;
  /** 周末清盘：周五 20:00 UTC 自动平掉该规则跟单端全部持仓。 */
  weekend_close: boolean;

  sl_mode: SlTpMode;
  sl_pips: number;
  tp_mode: SlTpMode;
  tp_pips: number;
  trade_delay_ms: number;
  skip_older_than_secs: number;

  trailing_pips: number;
  breakeven_after_pips: number;

  schedule: Schedule;
  pip_value_per_lot: number;

  /** Manual per-symbol SL/TP pip offset entries (captured via the Compare tab). */
  quote_offsets: QuoteOffset[];
}

export interface QuoteOffset {
  symbol: string;  // master-side ticker (uppercased)
  pips: number;    // signed pip shift applied to SL/TP
  /** Optional TV data-feed marker (`OANDA:`, `*PEPPERSTONE`). When set,
   *  the offset only matches trades from that feed; empty = match any. */
  feed?: string;
}

export interface EaStatus {
  platform: Platform;
  path: string;
  up_to_date: boolean;
  installed_bytes: number;
  bundled_bytes: number;
}

/// Status of the Cascada-managed TradingView sidecar (Python + mitmproxy).
/// Returned by the `tv_proxy_*` commands; powers the Accounts › TradingView
/// panel.
export interface TvProxyStatus {
  installed: boolean;        // venv + mitmproxy ready
  running: boolean;          // mitmdump child alive
  port: number;
  pythonPath?: string | null;
  pythonVersion?: string | null;
  addonPath?: string | null;
  venvPath?: string | null;
  certPath?: string | null;  // ~/.mitmproxy/mitmproxy-ca-cert.pem
  /** All preconditions for the bundled-browser flow are met (proxy
   * installed, cert generated + SPKI hashed, Chromium-family browser
   * found on disk). Drives the "Open TradingView" button. */
  browserReady: boolean;
  /** Resolved Chrome/Edge/Brave/Chromium binary; null when none found. */
  browserPath?: string | null;
  lastError?: string | null;
}

export interface Quote {
  account_id: string;
  symbol: string;
  bid: number;
  ask: number;
  /// Broker-reported pip size. 0/undefined when the EA hasn't been upgraded.
  pip_size?: number;
  /// EA-reported unrealised P&L (account currency) for all positions on this symbol.
  unrealized?: number;
  ts: number;
}

export function defaultRule(master_id = "", slave_id = ""): CopyRule {
  return {
    id: crypto.randomUUID(),
    name: "",
    master_id, slave_id,
    enabled: false,  // 默认不开启跟单，由用户手动启用
    lot_mode: "Multiplier", lot_value: 1,
    reverse: false,
    max_slippage_pips: 3, slave_magic: 0,
    symbol_map: {},
    min_lot: 0, max_lot: 0,
    master_min_lot: 0, master_max_lot: 0,
    master_magic_list: [],
    ignore_pending: false,
    order_comment: "", order_comment_src_lot: false,
    symbol_whitelist: [], symbol_blacklist: [],
    symbol_prefix: "", symbol_suffix: "",
    master_strip_prefix: "", master_strip_suffix: "",
    direction: "All",
    comment_filter: "",
    close_on_master_close: true,
    max_open_positions: 0, max_exposure_lots: 0, max_daily_loss: 0,
    max_floating_loss: 0, weekend_close: false,
    sl_mode: "Copy", sl_pips: 0,
    tp_mode: "Copy", tp_pips: 0,
    trade_delay_ms: 0, skip_older_than_secs: 0,
    trailing_pips: 0, breakeven_after_pips: 0,
    schedule: { enabled: false, start_min: 0, end_min: 24 * 60, skip_weekends: false },
    pip_value_per_lot: 10,
    quote_offsets: [],
  };
}

export interface Trade {
  ticket: string;
  account_id: string;
  symbol: string;
  side: "Buy" | "Sell";
  volume: number;
  price: number;
  sl: number | null;
  tp: number | null;
  opened_at: number;
  closed_at: number | null;
  profit: number | null;
  /** Broker-reported pip size. 0/undefined on pre-v0.1.6 EAs. */
  pip_size?: number;
  /** 信号端原始单号（从订单备注解析）。 */
  origin_ticket?: string | null;
  /** 产生该跟单订单的规则 ID（统计页按规则分组用），空 = 未关联。 */
  rule_id?: string;
}

export interface LogEntry {
  id: number;
  ts: number;
  level: "info" | "warn" | "error";
  source: string;
  message: string;
}

export const EVT = {
  log: "cascada://log",
  account: "cascada://account",
  trade: "cascada://trade",
  quote: "cascada://quote",
  symbols: "cascada://symbols",
} as const;

export const api = {
  listAccounts: () => invoke<Account[]>("list_accounts"),
  addAccount: (p: Omit<Account, "connected" | "balance" | "equity" | "currency" | "id"> & { password?: string }) =>
    invoke<Account>("add_account", { payload: p }),
  removeAccount: (id: string) => invoke<void>("remove_account", { id }),
  connectAccount: (id: string) => invoke<void>("connect_account", { id }),
  disconnectAccount: (id: string) => invoke<void>("disconnect_account", { id }),
  setRole: (id: string, role: AccountRole) => invoke<void>("set_role", { id, role }),
  renameAccount: (id: string, label: string) => invoke<void>("rename_account", { id, label }),

  listRules: () => invoke<CopyRule[]>("list_rules"),
  upsertRule: (rule: CopyRule) => invoke<CopyRule>("upsert_rule", { rule }),
  deleteRule: (id: string) => invoke<void>("delete_rule", { id }),
  /** 补单：让信号端重新上报持仓，缺失的跟单订单会被补开（忽略跟单时效）。 */
  resyncRule: (id: string) => invoke<string>("resync_rule", { id }),
  /** 清仓：平掉该规则产生的全部跟单持仓（并取消未成交挂单）。 */
  closeRulePositions: (id: string) => invoke<string>("close_rule_positions", { id }),
  /** 全局熔断：一键暂停/恢复所有规则，返回变更的规则数。 */
  setAllRulesEnabled: (enabled: boolean) => invoke<number>("set_all_rules_enabled", { enabled }),

  listTrades: () => invoke<Trade[]>("list_trades"),

  subscribeSymbols: (account_id: string, symbols: string[]) =>
    invoke<string[]>("subscribe_symbols", { accountId: account_id, symbols }),
  listQuotes: () => invoke<Quote[]>("list_quotes"),
  listSubscriptions: () => invoke<[string, string[]][]>("list_subscriptions"),
  requestSymbols: (account_id: string) =>
    invoke<boolean>("request_symbols", { accountId: account_id }),
  listAccountSymbols: (account_id: string) =>
    invoke<string[]>("list_symbols", { accountId: account_id }),

  installCtraderBot: () => invoke<string[]>("install_ctrader_bot"),
  installCtraderBotAt: (path: string) => invoke<string>("install_ctrader_bot_at", { path }),
  installMtEaAt: (platform: "MT4" | "MT5", path: string) =>
    invoke<string>("install_mt_ea_at", { platform, path }),
  installMtEa: (platform: "MT4" | "MT5") =>
    invoke<string[]>("install_mt_ea", { platform }),

  /** Compare installed EAs / cBots with the ones bundled in this build. */
  checkEaVersions: () => invoke<EaStatus[]>("check_ea_versions"),

  /** TradingView sidecar lifecycle. `setup` is idempotent — safe to re-run.
   * `openBrowser` ensures the proxy is running, then spawns an isolated
   * Chrome/Edge window pointed at TradingView (no system proxy changes). */
  tvProxyStatus: () => invoke<TvProxyStatus>("tv_proxy_status"),
  tvProxySetup: () => invoke<TvProxyStatus>("tv_proxy_setup"),
  tvProxyStart: () => invoke<TvProxyStatus>("tv_proxy_start"),
  tvProxyStop: () => invoke<TvProxyStatus>("tv_proxy_stop"),
  tvProxyOpenBrowser: () => invoke<TvProxyStatus>("tv_proxy_open_browser"),

  exportSettings: (path: string) => invoke<string>("export_settings", { path }),
  importSettings: (path: string) =>
    invoke<{ accounts: number; rules: number }>("import_settings", { path }),

  onEvent: (cb: (e: LogEntry) => void): Promise<UnlistenFn> =>
    listen<LogEntry>(EVT.log, (e) => cb(e.payload)),
  onAccountUpdate: (cb: (a: Account) => void): Promise<UnlistenFn> =>
    listen<Account>(EVT.account, (e) => cb(e.payload)),
  onTrade: (cb: (t: Trade) => void): Promise<UnlistenFn> =>
    listen<Trade>(EVT.trade, (e) => cb(e.payload)),
  onQuote: (cb: (q: Quote) => void): Promise<UnlistenFn> =>
    listen<Quote>(EVT.quote, (e) => cb(e.payload)),
  onSymbols: (cb: (account_id: string, symbols: string[]) => void): Promise<UnlistenFn> =>
    listen<[string, string[]]>(EVT.symbols, (e) => cb(e.payload[0], e.payload[1])),
};
