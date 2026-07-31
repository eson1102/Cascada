<script lang="ts">
  import { api, type Platform } from "../lib/api";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import TvProxyPanel from "./TvProxyPanel.svelte";

  let mode: Platform = "cTrader";
  let installStatus: { kind: "info" | "ok" | "err"; text: string } | null = null;

  const platforms: { id: Platform; name: string; tag: string }[] = [
    { id: "cTrader", name: "cTrader", tag: "cBot · 自动发现" },
    { id: "MT4",     name: "MetaTrader 4", tag: "EA · 自动发现" },
    { id: "MT5",     name: "MetaTrader 5", tag: "EA · 自动发现" },
    { id: "TradingView", name: "TradingView", tag: "Sidecar · 自动发现" },
  ];

  // Hide the shared install banner when TvProxyPanel surfaces its own
  // "Python missing" panel (avoids duplicate red text).
  $: pythonMissing = mode === "TradingView"
    && installStatus?.kind === "err"
    && /python.*not found on path/i.test(installStatus.text);

  async function installCtraderBot() {
    installStatus = { kind: "info", text: "正在扫描 cTrader 安装…" };
    try {
      const paths = await api.installCtraderBot();
      installStatus = { kind: "ok", text: `已安装到 ${paths.length} 个安装位置。确认 cTrader 的导入对话框，将 cBot 附加到图表并按下 Start——您的账户将自动出现在这里。` };
    } catch (e) {
      installStatus = { kind: "err", text: `${e} — 请改用"选择位置…"。` };
    }
  }
  async function installCtraderBotManual() {
    const picked = await openDialog({ directory: true, title: "选择您的 cAlgo 文件夹" });
    if (!picked || Array.isArray(picked)) return;
    installStatus = { kind: "info", text: "正在安装…" };
    try {
      const p = await api.installCtraderBotAt(picked);
      installStatus = { kind: "ok", text: `已安装 → ${p}。将 cBot 附加到图表并按下 Start。` };
    } catch (e) {
      installStatus = { kind: "err", text: `安装失败：${e}` };
    }
  }
  async function installMtEaAuto() {
    if (mode !== "MT4" && mode !== "MT5") return;
    const target: "MT4" | "MT5" = mode;
    installStatus = { kind: "info", text: `正在扫描 ${target} 终端…` };
    try {
      const paths = await api.installMtEa(target);
      installStatus = { kind: "ok", text: `EA 已安装到 ${paths.length} 个终端。刷新 ${target} 中的导航器面板，然后将 CascadaBridge 拖到图表上。` };
    } catch (e) {
      installStatus = { kind: "err", text: `${e}` };
    }
  }
  async function installMtEaManual() {
    if (mode !== "MT4" && mode !== "MT5") return;
    const target: "MT4" | "MT5" = mode;
    const picked = await openDialog({
      directory: true,
      title: `选择 ${target} 数据文件夹（包含 MQL${target === "MT4" ? "4" : "5"}/）`,
    });
    if (!picked || Array.isArray(picked)) return;
    installStatus = { kind: "info", text: "正在安装 EA…" };
    try {
      const p = await api.installMtEaAt(target, picked);
      installStatus = { kind: "ok", text: `EA 已复制 → ${p}。刷新 ${target} 中的导航器面板。` };
    } catch (e) {
      installStatus = { kind: "err", text: `安装失败：${e}` };
    }
  }

  function selectPlatform(id: Platform) {
    mode = id;
    installStatus = null;
  }
</script>

<div class="wizard">
  <div class="platforms">
    {#each platforms as p}
      <button class="plat-card" class:active={mode === p.id}
              on:click={() => selectPlatform(p.id)}>
        <div class="plat-badge-row">
          <span class="plat-badge {p.id}">{p.id}</span>
        </div>
        <span class="plat-name">{p.name}</span>
        <span class="plat-tag">{p.tag}</span>
      </button>
    {/each}
  </div>

  <div class="instructions">
    {#if mode === "cTrader"}
      <p class="lead">安装 <code>CascadaBridge</code> cBot，将其附加到任意图表并按下 <strong>Start</strong>。您的账户会自动出现在这里——无需登录或标签。</p>
      <div class="install-row">
        <button class="primary" on:click={installCtraderBot}>自动安装 cBot</button>
        <button on:click={installCtraderBotManual}>选择位置…</button>
      </div>
    {:else if mode === "TradingView"}
      <TvProxyPanel bind:installStatus active={mode === "TradingView"} />
    {:else}
      <p class="lead">
        安装 <code>CascadaBridge.{mode === "MT4" ? "mq4" : "mq5"}</code>，开启 <strong>AutoTrading</strong>，并将 EA 拖到任意图表上——您的账户会自动出现在这里。无需网络配置，支持同时运行多个 {mode} 终端。
      </p>
      <div class="install-row">
        <button class="primary" on:click={installMtEaAuto}>自动安装 Expert Advisor</button>
        <button on:click={installMtEaManual}>选择位置…</button>
      </div>
    {/if}
    {#if installStatus && !pythonMissing}
      <div class="inst-status {installStatus.kind}">{installStatus.text}</div>
    {/if}
  </div>
</div>

<style>
  .wizard {
    padding: 20px 22px 24px;
    border-bottom: 1px solid var(--border);
    background: linear-gradient(180deg, #fafbfc 0%, #ffffff 100%);
    display: flex; flex-direction: column; gap: 18px;
  }
  .platforms { display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 10px; }
  .plat-card {
    display: flex; flex-direction: column; gap: 4px;
    padding: 14px 16px;
    border: 1.5px solid var(--border); border-radius: 10px;
    background: #fff; text-align: left; cursor: pointer;
    transition: all 0.12s ease;
  }
  .plat-card:hover { border-color: #cbd5e1; transform: translateY(-1px); }
  .plat-card.active { border-color: var(--primary); background: var(--primary-soft); box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.08); }
  .plat-badge {
    align-self: flex-start;
    font-size: 10px; font-weight: 600; letter-spacing: 0.04em;
    padding: 2px 7px; border-radius: 4px;
    background: var(--surface-muted); color: var(--text-2);
  }
  .plat-badge.cTrader { background: #dbeafe; color: #1d4ed8; }
  .plat-badge.MT4     { background: #fef3c7; color: #a16207; }
  .plat-badge.MT5     { background: #dcfce7; color: #15803d; }
  .plat-badge.TradingView { background: #eff6ff; color: #1d4ed8; }
  .plat-badge-row { display: flex; align-items: center; gap: 6px; }
  .plat-name { font-size: 14px; font-weight: 600; color: var(--text); }
  .plat-tag  { font-size: 11px; color: var(--text-muted); }

  .instructions {
    padding: 14px 16px;
    background: var(--surface-muted);
    border: 1px solid var(--border);
    border-radius: 10px;
    display: flex; flex-direction: column; gap: 10px;
  }
  .lead { margin: 0; font-size: 13px; color: var(--text); }
  .lead code { background: #fff; padding: 1px 6px; border-radius: 4px; font-size: 12px; }
  .install-row { display: flex; gap: 8px; flex-wrap: wrap; }
  .inst-status {
    font-size: 12px; padding: 8px 12px; border-radius: 6px;
    border: 1px solid transparent;
  }
  .inst-status.info { background: #eff6ff; color: #1e40af; border-color: #bfdbfe; }
  .inst-status.ok   { background: #f0fdf4; color: #166534; border-color: #bbf7d0; }
  .inst-status.err  { background: #fef2f2; color: #991b1b; border-color: #fecaca; }
</style>
