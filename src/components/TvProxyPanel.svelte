<script lang="ts">
  import { onDestroy } from "svelte";
  import { api, type TvProxyStatus } from "../lib/api";

  // Bound from the parent so TV's status flows into the same banner
  // shared by the cTrader / MT installers (no duplicate UI surface).
  export let installStatus: { kind: "info" | "ok" | "err"; text: string } | null = null;
  // True only while this panel is mounted (i.e. TradingView tab open).
  // Drives the 2 s status poll — paused when inactive to save cycles.
  export let active: boolean = false;

  let tvStatus: TvProxyStatus | null = null;
  let tvBusy: "idle" | "setup" | "start" | "stop" | "open" = "idle";
  let tvPollTimer: ReturnType<typeof setInterval> | null = null;

  // Surface the "Python 3.10+ not found on PATH" failure as its own
  // panel (with a python.org link). String match is brittle on purpose
  // — the only place we generate it is detect_python().
  $: pythonMissing = installStatus?.kind === "err"
    && /python.*not found on path/i.test(installStatus.text);

  async function refreshTvStatus() {
    try { tvStatus = await api.tvProxyStatus(); }
    catch (e) { console.warn("[tv-proxy] status failed:", e); }
  }
  async function setupTvProxy() {
    if (tvBusy !== "idle") return;
    tvBusy = "setup";
    installStatus = { kind: "info", text: "正在安装…" };
    try {
      tvStatus = await api.tvProxySetup();
      installStatus = { kind: "ok", text: "代理已安装。点击“打开 TradingView”开始交易。" };
    } catch (e) {
      installStatus = { kind: "err", text: `${e}` };
    } finally { tvBusy = "idle"; }
  }
  async function openTvBrowser() {
    if (tvBusy !== "idle") return;
    tvBusy = "open";
    installStatus = { kind: "info", text: "正在隔离浏览器窗口中启动 TradingView…" };
    try {
      tvStatus = await api.tvProxyOpenBrowser();
      installStatus = { kind: "ok", text: "TradingView 已打开。随便下一笔小单 — 你的主账户会自动出现在这里。" };
    } catch (e) {
      installStatus = { kind: "err", text: `${e}` };
    } finally { tvBusy = "idle"; }
  }
  async function stopTvProxy() {
    if (tvBusy !== "idle") return;
    tvBusy = "stop";
    try {
      tvStatus = await api.tvProxyStop();
      installStatus = { kind: "info", text: "代理已停止。" };
    } catch (e) {
      installStatus = { kind: "err", text: `${e}` };
    } finally { tvBusy = "idle"; }
  }

  // Re-poll while the TradingView pane is active so the UI reflects
  // async setup/start completion (the supervisor task exits the child
  // without calling back into JS). 2 s is fast enough that the running
  // pill flips visibly within one frame of mitmdump exiting.
  $: if (active) {
    if (!tvPollTimer) {
      refreshTvStatus();
      tvPollTimer = setInterval(refreshTvStatus, 2000);
    }
  } else if (tvPollTimer) {
    clearInterval(tvPollTimer);
    tvPollTimer = null;
  }
  onDestroy(() => { if (tvPollTimer) clearInterval(tvPollTimer); });
</script>

<div class="tv-warning">
  <strong>正在积极开发中</strong> — 请谨慎使用。
</div>
<div class="tv-info">
  <strong>仅支持模拟盘。</strong> 此桥接支持 TradingView 的模拟盘 (PaperTrading) 账户。
  如需连接真实经纪商（FTMO、OANDA、Forex.com 等），请改用 <code>cTrader</code>
  桥接 — 它对非模拟账户更可靠。
</div>
<div class="tv-info">
  <strong>模拟盘功能有限。</strong> 在边界情况下（部分平仓、即时成交的挂单、净额合并、
  中间的止损/止盈帧），TV 的模拟账户表现与真实经纪商不同。<em>请先使用小额交易</em>
  做端到端功能测试，确认无误后再依赖它。若发现任何异常，建议以 <code>cTrader</code>
  或 <code>MT4</code>/<code>MT5</code> 作为主账户 — 这些经过实战检验，桥接语义稳定。
</div>
<div class="tv-info">
  <strong>净额 → 对冲映射。</strong> TV 模拟盘会把同一品种的所有开仓合并为一个净额持仓；
  从账户经纪商（MT4/MT5/cTrader）会把每一笔开仓视为<em>独立</em>持仓。
  在 TV 中连续两次各买入 1 BTC，会在从账户上产生两个独立的 1 BTC 持仓。
  止损/止盈的改动会作用于所有持仓；在 TV 中平仓会全部平掉。
</div>
<div class="tv-status">
  <span class="tv-pill {tvStatus?.running ? 'on' : tvStatus?.installed ? 'idle' : 'off'}">
    {tvStatus?.running ? `正在运行于端口 :${tvStatus.port}` : tvStatus?.installed ? "已安装 · 已停止" : "未安装"}
  </span>
  {#if tvStatus?.pythonVersion}
    <span class="muted small">Python {tvStatus.pythonVersion}</span>
  {/if}
  {#if tvStatus?.lastError}
    <span class="tv-err small" title={tvStatus.lastError}>上次错误：{tvStatus.lastError.length > 60 ? tvStatus.lastError.slice(0, 60) + "…" : tvStatus.lastError}</span>
  {/if}
</div>
{#if pythonMissing}
  <div class="py-required">
    <div class="py-required-head">
      <strong>需要 Python 3.10 或更高版本</strong>
      <span class="muted small">Cascada 使用 Python 运行 mitmproxy 辅助进程来与 TradingView 通信。</span>
    </div>
    <ol class="py-steps">
      <li>下载并安装 Python 3.10 或更新版本。</li>
      <li><strong>Windows 用户：</strong>在安装向导的第一个界面勾选 <em>“Add Python to PATH”</em> — 这是安装失败最常见的原因。</li>
      <li>回到此处并点击 <em>重试安装</em>。</li>
    </ol>
    <div class="install-row">
      <a class="btn-link primary" href="https://www.python.org/downloads/" target="_blank" rel="noreferrer">下载 Python →</a>
      <button on:click={setupTvProxy} disabled={tvBusy !== "idle"}>
        {tvBusy === "setup" ? "重试中…" : "重试安装"}
      </button>
    </div>
  </div>
{:else if tvStatus && !tvStatus.installed}
  <div class="install-row">
    <button class="primary" on:click={setupTvProxy} disabled={tvBusy !== "idle"}>
      {#if tvBusy === "setup"}<span class="spinner" aria-hidden="true"></span>正在安装…{:else}安装代理{/if}
    </button>
  </div>
  {#if tvBusy === "setup"}
    <div class="setup-progress">
      <div class="setup-progress-bar"><div class="setup-progress-fill"></div></div>
    </div>
  {/if}
{:else if tvStatus && !tvStatus.browserPath}
  <div class="browser-required">
    <div class="py-required-head">
      <strong>需要 Chrome、Edge 或 Brave</strong>
      <span class="muted small">Cascada 会在隔离窗口中启动 TradingView — 请选择任意基于 Chromium 的浏览器。</span>
    </div>
    <div class="install-row">
      <a class="btn-link primary" href="https://www.google.com/chrome/" target="_blank" rel="noreferrer">下载 Chrome →</a>
      <a class="btn-link" href="https://www.microsoft.com/edge" target="_blank" rel="noreferrer">下载 Edge</a>
      <button on:click={refreshTvStatus} disabled={tvBusy !== "idle"}>我已安装</button>
    </div>
  </div>
{:else}
  <div class="install-row">
    <button class="primary" on:click={openTvBrowser} disabled={tvBusy !== "idle" || !tvStatus?.browserReady}>
      {#if tvBusy === "open"}<span class="spinner" aria-hidden="true"></span>正在打开…{:else}打开 TradingView →{/if}
    </button>
    {#if tvStatus?.running}
      <button on:click={stopTvProxy} disabled={tvBusy !== "idle"} title="停止 mitmproxy 辅助进程（关闭代理，浏览器窗口保留）">
        {tvBusy === "stop" ? "正在停止…" : "停止代理"}
      </button>
    {/if}
    <button on:click={setupTvProxy} disabled={tvBusy !== "idle"} title="重新安装虚拟环境并刷新证书（很少用到）">
      重新安装
    </button>
  </div>
  <p class="hint">
    Cascada 将使用 {tvStatus?.browserPath?.includes("Edge") ? "Edge" : tvStatus?.browserPath?.includes("Brave") ? "Brave" : "Chrome"} 以及沙箱配置文件打开 TradingView。
    登录后随便下一笔小单 — 你的主账户会自动出现在这里。
  </p>
  <p class="hint">
    邮箱/密码以及 Google/Apple OAuth 均可正常使用 — Cascada 会将 Google + Apple
    认证域名绕过 mitmproxy，使 Chrome 接受提供方的真实证书。TradingView
    流量本身仍会被拦截。
  </p>
{/if}

<style>
  .tv-warning {
    display: block;
    margin-bottom: 12px;
    padding: 8px 12px;
    border-radius: 6px;
    background: linear-gradient(135deg, rgba(249, 115, 22, 0.12), rgba(245, 158, 11, 0.12));
    border-left: 3px solid #f97316;
    font-size: 13px;
    color: var(--text);
  }
  .tv-warning strong { color: #c2410c; }

  .tv-info {
    display: block;
    margin-bottom: 8px;
    padding: 8px 12px;
    border-radius: 6px;
    background: #eff6ff;
    border-left: 3px solid #3b82f6;
    font-size: 12px;
    color: var(--text);
    line-height: 1.5;
  }
  .tv-info strong { color: #1d4ed8; }
  .tv-info code {
    background: #fff; padding: 1px 6px; border-radius: 4px;
    font-size: 11px;
  }
  .tv-info em { color: #1d4ed8; font-style: normal; font-weight: 600; }

  .tv-status {
    display: flex; flex-wrap: wrap; align-items: center; gap: 10px;
    font-size: 12px;
  }
  .tv-pill {
    display: inline-flex; align-items: center; gap: 6px;
    font-weight: 600; font-size: 11px; letter-spacing: 0.02em;
    padding: 3px 9px; border-radius: 999px;
    border: 1px solid transparent;
  }
  .tv-pill::before {
    content: ""; width: 7px; height: 7px; border-radius: 50%;
    background: currentColor;
  }
  .tv-pill.on   { background: #f0fdf4; color: #166534; border-color: #bbf7d0; }
  .tv-pill.idle { background: #fef9c3; color: #92400e; border-color: #fde68a; }
  .tv-pill.off  { background: #f1f5f9; color: #475569; border-color: #cbd5e1; }
  .tv-err { color: #b91c1c; }

  .install-row { display: flex; gap: 8px; flex-wrap: wrap; }

  .py-required, .browser-required {
    display: flex; flex-direction: column; gap: 12px;
    padding: 14px 16px;
    border: 1px solid #FED7AA;
    background: linear-gradient(135deg, #FFF7ED, #FEFCE8);
    border-radius: 8px;
  }
  .py-required-head { display: flex; flex-direction: column; gap: 2px; }
  .py-required-head strong { font-size: 13px; color: #7C2D12; }
  .py-steps {
    margin: 0; padding-left: 22px;
    font-size: 13px; color: var(--text);
    line-height: 1.7;
  }
  .py-steps em { color: #7C2D12; font-style: normal; font-weight: 600; }
  .btn-link {
    display: inline-flex; align-items: center;
    padding: 8px 14px; border-radius: 6px;
    font-size: 13px; font-weight: 500;
    text-decoration: none;
  }
  .btn-link.primary { background: var(--primary); color: #fff; }
  .btn-link.primary:hover { filter: brightness(1.06); }

  .spinner {
    display: inline-block;
    width: 12px; height: 12px;
    margin-right: 8px;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
    vertical-align: -2px;
    opacity: 0.85;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .setup-progress {
    display: flex; flex-direction: column; gap: 6px;
    padding: 4px 0;
  }
  .setup-progress-bar {
    height: 4px; border-radius: 999px;
    background: var(--border);
    overflow: hidden;
  }
  .setup-progress-fill {
    height: 100%; width: 35%; border-radius: 999px;
    background: linear-gradient(90deg, var(--primary), #60a5fa);
    animation: setup-slide 1.4s ease-in-out infinite;
  }
  @keyframes setup-slide {
    0%   { transform: translateX(-100%); }
    100% { transform: translateX(380%); }
  }
</style>
