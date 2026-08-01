<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { fly, fade } from "svelte/transition";
  import { api, defaultRule, type Account, type CopyRule, type LotMode } from "../lib/api";
  import { accountIndex, labelOf, platformOf } from "../lib/format";
  import { ask } from "@tauri-apps/plugin-dialog";

  export let rules: CopyRule[];
  export let accounts: Account[];
  const dispatch = createEventDispatcher();
  $: idx = accountIndex(accounts);
  $: masters = accounts.filter((a) => a.role === "Master");
  $: slaves  = accounts.filter((a) => a.role === "Slave" || a.role === "Idle");

  // Build the option lists for the edit drawer — always include the currently
  // bound account (even if its role changed), so editing a rule whose master
  // was demoted still shows that master in the dropdown rather than blanking
  // the selection on open and refusing to save.
  function masterOptions(currentId: string): Account[] {
    const list = masters.slice();
    if (currentId && !list.some((a) => a.id === currentId)) {
      const cur = idx.get(currentId);
      if (cur) list.push(cur);
    }
    return list;
  }
  function slaveOptions(currentId: string): Account[] {
    const list = slaves.slice();
    if (currentId && !list.some((a) => a.id === currentId)) {
      const cur = idx.get(currentId);
      if (cur) list.push(cur);
    }
    return list;
  }

  function roleLabel(role: string): string {
    if (role === "Master") return "信号端";
    if (role === "Slave") return "跟单端";
    if (role === "Idle") return "空闲 (idle)";
    return role;
  }

  function ruleIssue(r: CopyRule): string | null {
    const m = idx.get(r.master_id);
    const s = idx.get(r.slave_id);
    if (!m) return "缺少信号端";
    if (!s) return "缺少跟单端";
    if (m.role !== "Master") return `信号端现在是 ${m.role}`;
    if (s.role !== "Slave") return `跟单端现在是 ${s.role}`;
    return null;
  }

  let editing: CopyRule | null = null;
  type TabId = "lot" | "filters" | "risk" | "orders" | "schedule" | "advanced";
  let activeTab: TabId = "lot";

  let mapPairs: [string, string][] = [];
  function loadMap(r: CopyRule) {
    mapPairs = Object.entries(r.symbol_map ?? {});
  }
  function syncMap() {
    if (!editing) return;
    // Preserve broker case on both sides — some brokers use suffix tickers
    // like `XAUUSDb` where the lowercase `b` is significant. The engine
    // does a case-insensitive fallback when an exact-case map miss happens,
    // so capitalising here would only get in the way.
    const out: Record<string, string> = {};
    for (const [k, v] of mapPairs) {
      const kk = k.trim();
      const vv = v.trim();
      if (kk && vv) out[kk] = vv;
    }
    editing.symbol_map = out;
  }
  function addMapping() { mapPairs = [...mapPairs, ["", ""]]; }
  function removeMapping(i: number) {
    mapPairs = mapPairs.filter((_, j) => j !== i);
    syncMap();
  }
  function updateMapping(i: number, side: 0 | 1, value: string) {
    mapPairs[i][side] = value;
    mapPairs = mapPairs;
    syncMap();
  }

  function newDraft() { editing = defaultRule(); activeTab = "lot"; loadMap(editing); }
  function editRule(r: CopyRule) { editing = JSON.parse(JSON.stringify(r)); activeTab = "lot"; loadMap(editing!); }
  function cancel() { editing = null; mapPairs = []; }

  async function save() {
    if (!editing) return;
    if (!editing.master_id || !editing.slave_id) return;
    await api.upsertRule(editing);
    editing = null;
    dispatch("refresh");
  }
  async function toggle(r: CopyRule) {
    await api.upsertRule({ ...r, enabled: !r.enabled });
    dispatch("refresh");
  }
  async function remove(r: CopyRule) {
    const m = idx.get(r.master_id);
    const s = idx.get(r.slave_id);
    const pair = m && s ? ` (${labelOf(idx, r.master_id)} → ${labelOf(idx, r.slave_id)})` : "";
    const name = r.name?.trim() || "此规则";
    const ok = await ask(
      `删除“${name}”${pair}？\n\n此后信号端的新交易将不再通过此规则复制到该跟单端。现有持仓不受影响。`,
      { title: "删除复制规则？", kind: "warning", okLabel: "删除", cancelLabel: "取消" });
    if (!ok) return;
    await api.deleteRule(r.id);
    dispatch("refresh");
  }

  // 补单：请求信号端重新上报持仓，缺失的跟单订单会被补开（忽略跟单时效）。
  let resyncingId: string | null = null;
  async function resync(r: CopyRule) {
    if (resyncingId) return;
    const m = idx.get(r.master_id);
    const master = m ? labelOf(idx, r.master_id) : r.master_id;
    const name = r.name?.trim() || "此规则";
    const ok = await ask(
      `补单“${name}”（信号端 ${master}）？\n\n将把信号端当前所有持仓与跟单端比对，缺失的订单立即补开（不受跟单时效限制），已跟单的自动跳过。`,
      { title: "确认补单", kind: "info", okLabel: "补单", cancelLabel: "取消" });
    if (!ok) return;
    resyncingId = r.id;
    try {
      const msg = await api.resyncRule(r.id);
      const { message } = await import("@tauri-apps/plugin-dialog");
      await message(msg, { title: "补单", kind: "info", okLabel: "好" });
    } catch (e) {
      const { message } = await import("@tauri-apps/plugin-dialog");
      await message(String(e), { title: "补单失败", kind: "error", okLabel: "好" });
    } finally {
      resyncingId = null;
    }
  }

  // 清仓：平掉该规则产生的全部跟单持仓（并取消未成交挂单）。
  let closingId: string | null = null;
  async function closePositions(r: CopyRule) {
    if (closingId) return;
    const name = r.name?.trim() || "此规则";
    const ok = await ask(
      `清仓“${name}”？\n\n将平掉该规则产生的全部跟单持仓，并取消未成交的挂单。此操作不可撤销。`,
      { title: "确认清仓", kind: "warning", okLabel: "清仓", cancelLabel: "取消" });
    if (!ok) return;
    closingId = r.id;
    try {
      const msg = await api.closeRulePositions(r.id);
      const { message } = await import("@tauri-apps/plugin-dialog");
      await message(msg, { title: "清仓", kind: "info", okLabel: "好" });
    } catch (e) {
      const { message } = await import("@tauri-apps/plugin-dialog");
      await message(String(e), { title: "清仓失败", kind: "error", okLabel: "好" });
    } finally {
      closingId = null;
    }
  }

  function csvBind(arr: string[]): string { return arr.join(", "); }
  function fromCsv(s: string): string[] {
    return s.split(",").map((x) => x.trim()).filter(Boolean);
  }
  function minToHHMM(m: number): string {
    const h = Math.floor(m / 60), mm = m % 60;
    return `${String(h).padStart(2, "0")}:${String(mm).padStart(2, "0")}`;
  }
  function hhmmToMin(s: string): number {
    const [h, m] = s.split(":").map((x) => parseInt(x, 10));
    return (isFinite(h) ? h : 0) * 60 + (isFinite(m) ? m : 0);
  }

  const LOT_MODES: { id: LotMode; label: string; hint: string; icon: string }[] = [
    { id: "Multiplier",   label: "倍数",           hint: "跟单端 = 信号端 × 值",              icon: "✕" },
    { id: "Fixed",        label: "固定手数",       hint: "始终开 `value` 手",                 icon: "▣" },
    { id: "Equity",       label: "净值比例",       hint: "按净值比例 × 值缩放",               icon: "≈" },
    { id: "BalanceRatio", label: "余额比例",       hint: "按余额比例 × 值缩放",               icon: "⚖" },
    { id: "RiskPercent",  label: "风险比例",       hint: "按止损距离相对净值 % 计算手数",     icon: "%" },
  ];

  type Chip = { kind: "info" | "warn" | "danger" | "primary"; text: string };
  function chipsForRule(r: CopyRule): Chip[] {
    const out: Chip[] = [];
    out.push({ kind: "primary", text: r.lot_mode === "RiskPercent" ? `${r.lot_value}% 风险` : `${LOT_MODES.find((m) => m.id === r.lot_mode)?.label ?? r.lot_mode} ×${r.lot_value}` });
    if (r.reverse) out.push({ kind: "warn", text: "反向" });
    if (r.direction !== "All") out.push({ kind: "info", text: r.direction === "BuyOnly" ? "仅买入" : "仅卖出" });
    if (r.symbol_whitelist.length) out.push({ kind: "info", text: `白名单 · ${r.symbol_whitelist.length}` });
    if (r.symbol_blacklist.length) out.push({ kind: "warn", text: `黑名单 · ${r.symbol_blacklist.length}` });
    if (r.symbol_prefix || r.symbol_suffix) out.push({ kind: "info", text: `${r.symbol_prefix}…${r.symbol_suffix}` });
    const mapN = Object.keys(r.symbol_map ?? {}).length;
    if (mapN) out.push({ kind: "info", text: `映射 · ${mapN}` });
    if (r.sl_mode !== "Copy") out.push({ kind: "info", text: `止损 ${r.sl_mode === "Fixed" ? r.sl_pips + "p" : "关"}` });
    if (r.tp_mode !== "Copy") out.push({ kind: "info", text: `止盈 ${r.tp_mode === "Fixed" ? r.tp_pips + "p" : "关"}` });
    if (r.trailing_pips)       out.push({ kind: "info", text: `移动止损 ${r.trailing_pips}p` });
    if (r.breakeven_after_pips)out.push({ kind: "info", text: `保本 +${r.breakeven_after_pips}p` });
    if (r.max_slippage_pips)   out.push({ kind: "info", text: `滑点 ≤ ${r.max_slippage_pips}p` });
    if (r.min_lot)             out.push({ kind: "info", text: `最小 ${r.min_lot} 手` });
    if (r.max_lot)             out.push({ kind: "info", text: `最大 ${r.max_lot} 手` });
    if (r.master_min_lot)      out.push({ kind: "info", text: `信号端 ≥ ${r.master_min_lot} 手` });
    if (r.master_max_lot)      out.push({ kind: "info", text: `信号端 ≤ ${r.master_max_lot} 手` });
    if (r.master_magic_list?.length)
      out.push({ kind: "info", text: `魔术号 ${r.master_magic_list.join(", ")}` });
    if (r.order_comment?.trim()) out.push({ kind: "primary", text: `备注 ${r.order_comment.trim()}` });
    if (r.order_comment_src_lot) out.push({ kind: "primary", text: "备注含信号端手数" });
    const offN = r.quote_offsets?.length ?? 0;
    if (offN)                  out.push({ kind: "info", text: `偏差 · ${offN}` });
    if (r.comment_filter)      out.push({ kind: "info", text: `备注 “${r.comment_filter}”` });
    if (r.skip_older_than_secs)out.push({ kind: "info", text: `跳过 >${r.skip_older_than_secs}s` });
    if (r.max_open_positions) out.push({ kind: "info", text: `≤ ${r.max_open_positions} 持仓` });
    if (r.max_exposure_lots)  out.push({ kind: "info", text: `≤ ${r.max_exposure_lots} 手` });
    if (r.max_daily_loss)     out.push({ kind: "danger", text: `−${r.max_daily_loss} 触发停止` });
    if (r.schedule.enabled)   out.push({ kind: "info", text: `${minToHHMM(r.schedule.start_min)}–${minToHHMM(r.schedule.end_min)}` });
    if (r.schedule.skip_weekends) out.push({ kind: "info", text: "仅工作日" });
    if (r.trade_delay_ms)     out.push({ kind: "info", text: `+${r.trade_delay_ms}ms` });
    return out;
  }

  // Single pass builds chips + issue string per rule, so we don't iterate
  // `rules` once for chips, once for issuesCount, then call ruleIssue() a
  // third time inside the template.
  $: ruleMeta = (() => {
    const m = new Map<string, { chips: Chip[]; issue: string | null }>();
    let issues = 0;
    for (const r of rules) {
      const issue = ruleIssue(r);
      if (issue) issues++;
      m.set(r.id, { chips: chipsForRule(r), issue });
    }
    return { map: m, issues };
  })();
  $: chipsByRule = new Map([...ruleMeta.map].map(([k, v]) => [k, v.chips]));
  $: issuesCount = ruleMeta.issues;

  const TABS: { id: TabId; label: string; icon: string; desc: string }[] = [
    { id: "lot",      label: "手数设置",   icon: "⚖", desc: "跟单端手数的计算方式" },
    { id: "filters",  label: "筛选",       icon: "⛃", desc: "复制哪些交易与品种" },
    { id: "risk",     label: "风险上限",   icon: "🛡", desc: "每个跟单端的安全限制" },
    { id: "orders",   label: "订单设置",   icon: "✎", desc: "止损/止盈、滑点、延迟" },
    { id: "schedule", label: "时段",       icon: "⏱", desc: "每日交易时段" },
    { id: "advanced", label: "高级",       icon: "⚙", desc: "移动止损与保本" },
  ];
</script>

<div class="card rules-root">
  <div class="card-header">
    <div class="header-left">
      <h2>复制规则</h2>
      <span class="count-pill">{rules.length}</span>
    </div>
    <button class="primary btn-new" on:click={newDraft} disabled={!!editing}>
      <span class="plus">+</span> 新建规则
    </button>
  </div>

  {#if issuesCount > 0}
    <div class="banner-warn">
      <span class="banner-icon">⚠</span>
      <div class="banner-body">
        <div class="banner-title">{issuesCount} 条规则需要您处理</div>
        <div class="banner-sub">规则引用的信号端或跟单端已不再匹配规则要求的角色。请在下方重新指派或删除受影响的规则。</div>
      </div>
    </div>
  {/if}

  {#if rules.length === 0}
    <div class="empty-state">
      <div class="empty-glyph">⇄</div>
      <h3 class="empty-title">还没有复制规则</h3>
      <p class="empty-sub">
        在账户 (Accounts) 标签页中将一个账户标记为<b>信号端</b>、另一个标记为<b>跟单端</b>，
        然后在此创建规则以开始镜像交易。
      </p>
      <button class="primary" on:click={newDraft}>+ 创建第一条规则</button>
    </div>
  {:else}
    <div class="rule-list">
      {#each rules as r (r.id)}
        {@const meta = ruleMeta.map.get(r.id)}
        {@const chips = meta?.chips ?? []}
        {@const issue = meta?.issue ?? null}
        <div class="rule" class:off={!r.enabled} class:warn={!!issue}>
          <div class="rule-status-bar" class:on={r.enabled} class:warn={!!issue}></div>

          <div class="rule-main">
            <div class="rule-name-row">
              <h4 class="rule-name" class:untitled={!r.name?.trim()}>
                {r.name?.trim() || "未命名规则"}
              </h4>
              {#if issue}
                <span class="warn-pill" title={issue}>⚠ {issue}</span>
              {/if}
            </div>

            <div class="rule-flow">
              <div class="acc-block">
                <span class="role-tag master">信号端</span>
                <div class="acc-line">
                  <span class="chip platform {platformOf(idx, r.master_id)}">{platformOf(idx, r.master_id)}</span>
                  <span class="acc-label">{labelOf(idx, r.master_id)}</span>
                </div>
              </div>

              <div class="flow-arrow" aria-hidden="true">
                <span class="arrow-line"></span>
                <span class="arrow-head">▶</span>
              </div>

              <div class="acc-block">
                <span class="role-tag slave">跟单端</span>
                <div class="acc-line">
                  <span class="chip platform {platformOf(idx, r.slave_id)}">{platformOf(idx, r.slave_id)}</span>
                  <span class="acc-label">{labelOf(idx, r.slave_id)}</span>
                </div>
              </div>
            </div>

            <div class="rule-chips">
              {#each chips as c}
                <span class="cfg-chip {c.kind}">{c.text}</span>
              {/each}
            </div>
          </div>

          <div class="rule-actions">
            <button class="toggle" class:on={r.enabled}
                    title={r.enabled ? "暂停复制" : "恢复复制"}
                    on:click={() => toggle(r)}>
              <span class="toggle-track"><span class="toggle-thumb"></span></span>
              <span class="toggle-label">{r.enabled ? "运行中" : "已暂停"}</span>
            </button>
            <button class="edit-btn" title="编辑规则" on:click={() => editRule(r)}>
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9M16.5 3.5a2.12 2.12 0 013 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>
              <span>编辑</span>
            </button>
            <button class="resync-btn" class:busy={resyncingId === r.id}
                    title="补单：补上信号端持仓中缺失的跟单订单（忽略跟单时效）"
                    disabled={resyncingId !== null} on:click={() => resync(r)}>
              <span class="resync-dot"></span>
              <span>{resyncingId === r.id ? "补单中…" : "补单"}</span>
            </button>
            <button class="close-btn" class:busy={closingId === r.id}
                    title="清仓：平掉该规则产生的全部跟单持仓"
                    disabled={closingId !== null} on:click={() => closePositions(r)}>
              <span class="close-dot"></span>
              <span>{closingId === r.id ? "清仓中…" : "清仓"}</span>
            </button>
            <button class="icon-btn danger" title="删除规则" on:click={() => remove(r)}>✕</button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

{#if editing}
  {@const e = editing}
  {@const isEdit = rules.some((r) => r.id === e.id)}
  <div class="overlay" on:click={cancel} transition:fade={{ duration: 120 }}></div>
  <aside class="drawer" transition:fly={{ x: 480, duration: 220 }}>
    <header class="drawer-head">
      <div class="drawer-head-main">
        <div class="drawer-eyebrow">{isEdit ? "编辑规则" : "新建规则"}</div>
        <input
          type="text"
          class="drawer-name"
          placeholder="未命名规则"
          bind:value={editing.name}
        />
      </div>
      <button class="icon-btn close" on:click={cancel} title="关闭">✕</button>
    </header>

    <div class="drawer-pair">
      <label class="pair-block">
        <span class="pair-label">信号端</span>
        <select class="pair-select" bind:value={editing.master_id}>
          <option value="" disabled>选择信号端…</option>
          {#each masterOptions(editing.master_id) as a}
            <option value={a.id}>{a.label} ({a.platform}){a.role !== "Master" ? ` — ${roleLabel(a.role)}` : ""}</option>
          {/each}
        </select>
      </label>
      <div class="pair-arrow">→</div>
      <label class="pair-block">
        <span class="pair-label">跟单端</span>
        <select class="pair-select" bind:value={editing.slave_id}>
          <option value="" disabled>选择跟单端…</option>
          {#each slaveOptions(editing.slave_id) as a}
            <option value={a.id}>{a.label} ({a.platform}){a.role === "Master" ? " — 信号端" : ""}</option>
          {/each}
        </select>
      </label>
      <label class="pair-toggle" class:on={editing.enabled}>
        <input type="checkbox" bind:checked={editing.enabled} />
        <span class="toggle-track"><span class="toggle-thumb"></span></span>
        <span>{editing.enabled ? "运行中" : "已暂停"}</span>
      </label>
    </div>

    <div class="drawer-body">
      <nav class="vtabs">
        {#each TABS as t}
          <button class="vtab" class:on={activeTab === t.id} on:click={() => (activeTab = t.id)}>
            <span class="vtab-icon">{t.icon}</span>
            <span class="vtab-text">
              <span class="vtab-label">{t.label}</span>
              <span class="vtab-desc">{t.desc}</span>
            </span>
          </button>
        {/each}
      </nav>

      <section class="vbody">
        {#if activeTab === "lot"}
          <header class="sec-head">
            <h3 class="section-title">手数设置</h3>
            <p class="section-sub">选择跟单端手数如何由信号端派生。</p>
          </header>

          <div class="radio-grid">
            {#each LOT_MODES as m}
              <label class="radio-card" class:on={editing.lot_mode === m.id}>
                <input type="radio" bind:group={editing.lot_mode} value={m.id} />
                <span class="rc-icon">{m.icon}</span>
                <span class="rc-text">
                  <span class="rc-title">{m.label}</span>
                  <span class="rc-hint">{m.hint}</span>
                </span>
              </label>
            {/each}
          </div>

          <div class="form-grid mt">
            <div class="field">
              <label class="f-label" for="lot-value">
                {editing.lot_mode === "RiskPercent" ? "每笔交易风险" : "手数值"}
              </label>
              <div class="input-suffix">
                <input id="lot-value" type="number" step="0.01" min="0" bind:value={editing.lot_value} />
                <span class="suffix">{editing.lot_mode === "RiskPercent" ? "%" : editing.lot_mode === "Fixed" ? "lots" : "×"}</span>
              </div>
              <p class="f-help">
                {editing.lot_mode === "Fixed" ? "始终精确开启这么多手。"
                  : editing.lot_mode === "RiskPercent" ? "每笔交易动用跟单端净值的百分比。"
                  : "在所选模式之上应用的缩放系数。"}
              </p>
            </div>
            {#if editing.lot_mode === "RiskPercent"}
              <div class="field">
                <label class="f-label" for="pip-value">每手点值</label>
                <div class="input-suffix">
                  <input id="pip-value" type="number" step="0.01" min="0.01" bind:value={editing.pip_value_per_lot} />
                  <span class="suffix">$/pip</span>
                </div>
                <p class="f-help">用于将风险比例换算为手数。</p>
              </div>
            {/if}
            <div class="field">
              <label class="f-label" for="min-lot">最小手数</label>
              <div class="input-suffix">
                <input id="min-lot" type="number" step="0.01" min="0" bind:value={editing.min_lot} />
                <span class="suffix">lots</span>
              </div>
              <p class="f-help">0 = 不设下限。</p>
            </div>
            <div class="field">
              <label class="f-label" for="max-lot">最大手数</label>
              <div class="input-suffix">
                <input id="max-lot" type="number" step="0.01" min="0" bind:value={editing.max_lot} />
                <span class="suffix">lots</span>
              </div>
              <p class="f-help">0 = 不设上限。</p>
            </div>
          </div>

          <label class="check-row mt">
            <input type="checkbox" bind:checked={editing.reverse} />
            <span class="check-text">
              <strong>反向 (reverse)</strong>
              <span class="muted">在跟单端上镜像 买入 ↔ 卖出。</span>
            </span>
          </label>

        {:else if activeTab === "filters"}
          <header class="sec-head">
            <h3 class="section-title">筛选</h3>
            <p class="section-sub">决定哪些信号端交易会复制到跟单端。</p>
          </header>

          <div class="form-grid">
            <div class="field">
              <label class="f-label" for="direction">方向</label>
              <select id="direction" bind:value={editing.direction}>
                <option value="All">买入和卖出均可</option>
                <option value="BuyOnly">仅买入</option>
                <option value="SellOnly">仅卖出</option>
              </select>
            </div>
            <div class="field">
              <label class="f-label" for="comment">信号端备注筛选</label>
              <input id="comment" type="text" placeholder="例如 Scalper#1" bind:value={editing.comment_filter} />
              <p class="f-help">按信号端订单自带的备注过滤，不区分大小写子串匹配。留空 = 不过滤。（注意：这与"订单设置"里写入跟单订单的备注不同）</p>
            </div>
          </div>

          <h4 class="sub-section">信号端手数过滤</h4>
          <p class="section-sub">按信号端<strong>原始</strong>下单手数筛选要复制的订单，不满足直接跳过（区别于上方"手数设置"里的最小/最大手数限制）。</p>
          <div class="form-grid">
            <div class="field">
              <label class="f-label" for="master-min-lot">信号端最小手数</label>
              <div class="input-suffix">
                <input id="master-min-lot" type="number" step="0.01" min="0" bind:value={editing.master_min_lot} />
                <span class="suffix">lots</span>
              </div>
              <p class="f-help">只复制手数 ≥ 此值的订单。0 = 不设下限。</p>
            </div>
            <div class="field">
              <label class="f-label" for="master-max-lot">信号端最大手数</label>
              <div class="input-suffix">
                <input id="master-max-lot" type="number" step="0.01" min="0" bind:value={editing.master_max_lot} />
                <span class="suffix">lots</span>
              </div>
              <p class="f-help">只复制手数 ≤ 此值的订单。0 = 不设上限。</p>
            </div>
          </div>
          <div class="hint-box">
            <span class="hint-icon">ℹ</span>
            <span>例如 0.03 与 0.5 时，仅复制 0.03 &lt; 手数 &lt; 0.5 的信号端订单。</span>
          </div>

          <h4 class="sub-section">魔术号过滤</h4>
          <p class="section-sub">只复制信号端订单魔术号 (Magic Number) 与列表匹配的交易。cTrader / TradingView 账户无魔术号概念。</p>
          <div class="field full">
            <label class="f-label" for="magic-list">魔术号</label>
            <input id="magic-list" type="text" placeholder="例如 12345, 67890"
              value={editing.master_magic_list.join(", ")}
              on:input={(ev) => editing && (editing.master_magic_list = ev.currentTarget.value.split(/[,，\s]+/).map((s) => s.trim()).filter((s) => s !== "" && !isNaN(Number(s))).map(Number))} />
            <p class="f-help">逗号分隔的精确魔术号，匹配任意一个即复制。留空 = 不过滤。适用于按 EA 区分信号来源。</p>
          </div>

          <label class="check-row mt">
            <input type="checkbox" bind:checked={editing.close_on_master_close} />
            <span class="check-text">
              <strong>信号端平仓时同步平仓</strong>
              <span class="muted">将信号端的平仓与挂单取消镜像到跟单端。关闭后由跟单端自行管理出场。</span>
            </span>
          </label>

          <h4 class="sub-section">品种匹配</h4>
          <div class="form-grid">
            <div class="field full">
              <label class="f-label" for="wl">白名单</label>
              <input id="wl" type="text" placeholder="EUR, XAU, GER40"
                value={csvBind(editing.symbol_whitelist)}
                on:input={(ev) => editing && (editing.symbol_whitelist = fromCsv(ev.currentTarget.value))} />
              <p class="f-help">逗号分隔的子串。留空 = 全部允许。</p>
            </div>
            <div class="field full">
              <label class="f-label" for="bl">黑名单</label>
              <input id="bl" type="text" placeholder="USDJPY, BTC"
                value={csvBind(editing.symbol_blacklist)}
                on:input={(ev) => editing && (editing.symbol_blacklist = fromCsv(ev.currentTarget.value))} />
              <p class="f-help">匹配其中任意一项的交易将被跳过。</p>
            </div>
            <div class="field">
              <label class="f-label" for="strip-prefix">信号端剥离前缀</label>
              <input id="strip-prefix" type="text" placeholder="（无）" bind:value={editing.master_strip_prefix} />
              <p class="f-help">先从信号端代码中移除（不区分大小写）。</p>
            </div>
            <div class="field">
              <label class="f-label" for="strip-suffix">信号端剥离后缀</label>
              <input id="strip-suffix" type="text" placeholder="m" bind:value={editing.master_strip_suffix} />
              <p class="f-help">先从信号端代码中移除（不区分大小写）。</p>
            </div>
            <div class="field">
              <label class="f-label" for="prefix">跟单端前缀</label>
              <input id="prefix" type="text" placeholder="（无）" bind:value={editing.symbol_prefix} />
              <p class="f-help">添加到最终跟单端代码的前面。</p>
            </div>
            <div class="field">
              <label class="f-label" for="suffix">跟单端后缀</label>
              <input id="suffix" type="text" placeholder=".r" bind:value={editing.symbol_suffix} />
              <p class="f-help">追加到最终跟单端代码的末尾。</p>
            </div>
          </div>
          <div class="hint-box">
            <span class="hint-icon">→</span>
            <span>
              信号端 <code>{editing.master_strip_prefix || ""}EURUSD{editing.master_strip_suffix || ""}</code>
              → 跟单端
              <code>{editing.symbol_prefix || ""}EURUSD{editing.symbol_suffix || ""}</code>
              （下方精确映射优先）。
            </span>
          </div>

          <h4 class="sub-section">品种覆盖</h4>
          <p class="section-sub">信号端 → 跟单端的精确映射。当信号端品种匹配时优先于前缀/后缀。</p>
          {#if mapPairs.length === 0}
            <p class="f-help" style="margin: 6px 0 10px;">暂无覆盖。若跟单端经纪商使用不同代码，可添加一条（例如 <code>XAUUSD</code> → <code>GOLD.r</code>）。</p>
          {:else}
            <div class="map-list">
              {#each mapPairs as pair, i (i)}
                <div class="map-row">
                  <input type="text" placeholder="信号端 (例如 XAUUSD)"
                    value={pair[0]}
                    on:input={(ev) => updateMapping(i, 0, ev.currentTarget.value)} />
                  <span class="map-arrow">→</span>
                  <input type="text" placeholder="跟单端 (例如 GOLD.r)"
                    value={pair[1]}
                    on:input={(ev) => updateMapping(i, 1, ev.currentTarget.value)} />
                  <button type="button" class="map-remove" title="移除" on:click={() => removeMapping(i)}>✕</button>
                </div>
              {/each}
            </div>
          {/if}
          <button type="button" class="map-add" on:click={addMapping}>+ 添加映射</button>

        {:else if activeTab === "risk"}
          <header class="sec-head">
            <h3 class="section-title">风险上限</h3>
            <p class="section-sub">下单前执行的硬性限制，超出即跳过。</p>
          </header>

          <div class="form-grid">
            <div class="field">
              <label class="f-label" for="max-pos">最大持仓数</label>
              <div class="input-suffix">
                <input id="max-pos" type="number" min="0" step="1" bind:value={editing.max_open_positions} />
                <span class="suffix">个</span>
              </div>
              <p class="f-help">0 = 不限。</p>
            </div>
            <div class="field">
              <label class="f-label" for="max-exp">最大总敞口</label>
              <div class="input-suffix">
                <input id="max-exp" type="number" min="0" step="0.01" bind:value={editing.max_exposure_lots} />
                <span class="suffix">lots</span>
              </div>
              <p class="f-help">跟单端当前持仓手数之和。</p>
            </div>
            <div class="field">
              <label class="f-label" for="max-loss">最大日亏损</label>
              <div class="input-suffix">
                <input id="max-loss" type="number" min="0" step="1" bind:value={editing.max_daily_loss} />
                <span class="suffix">货币</span>
              </div>
              <p class="f-help">超过此值后停止新复制。0 = 关闭。</p>
            </div>
            <div class="field">
              <label class="f-label" for="max-age">跳过早于以下时间的交易</label>
              <div class="input-suffix">
                <input id="max-age" type="number" min="0" step="1" bind:value={editing.skip_older_than_secs} />
                <span class="suffix">s</span>
              </div>
              <p class="f-help">避免复制过期成交。0 = 关闭。</p>
            </div>
          </div>
          <div class="hint-box">
            <span class="hint-icon">ℹ</span>
            <span>日亏损统计跟单端自 00:00 UTC 起的已平仓交易。</span>
          </div>

        {:else if activeTab === "orders"}
          <header class="sec-head">
            <h3 class="section-title">订单设置</h3>
            <p class="section-sub">止损/止盈、滑点限制与跟单延迟。</p>
          </header>

          <h4 class="sub-section">止损</h4>
          <div class="form-grid">
            <div class="field">
              <label class="f-label" for="sl-mode">模式</label>
              <select id="sl-mode" bind:value={editing.sl_mode}>
                <option value="Copy">复制信号端</option>
                <option value="Ignore">忽略（无止损）</option>
                <option value="Fixed">固定距离</option>
              </select>
            </div>
            {#if editing.sl_mode === "Fixed"}
              <div class="field">
                <label class="f-label" for="sl-pips">止损距离</label>
                <div class="input-suffix">
                  <input id="sl-pips" type="number" min="0" step="0.1" bind:value={editing.sl_pips} />
                  <span class="suffix">pips</span>
                </div>
              </div>
            {/if}
          </div>

          <h4 class="sub-section">止盈</h4>
          <div class="form-grid">
            <div class="field">
              <label class="f-label" for="tp-mode">模式</label>
              <select id="tp-mode" bind:value={editing.tp_mode}>
                <option value="Copy">复制信号端</option>
                <option value="Ignore">忽略（无止盈）</option>
                <option value="Fixed">固定距离</option>
              </select>
            </div>
            {#if editing.tp_mode === "Fixed"}
              <div class="field">
                <label class="f-label" for="tp-pips">止盈距离</label>
                <div class="input-suffix">
                  <input id="tp-pips" type="number" min="0" step="0.1" bind:value={editing.tp_pips} />
                  <span class="suffix">pips</span>
                </div>
              </div>
            {/if}
          </div>

          <h4 class="sub-section">执行</h4>
          <div class="form-grid">
            <div class="field">
              <label class="f-label" for="slip">最大滑点</label>
              <div class="input-suffix">
                <input id="slip" type="number" min="0" step="1" bind:value={editing.max_slippage_pips} />
                <span class="suffix">pips</span>
              </div>
            </div>
            <div class="field">
              <label class="f-label" for="delay">跟单延迟</label>
              <div class="input-suffix">
                <input id="delay" type="number" min="0" step="50" bind:value={editing.trade_delay_ms} />
                <span class="suffix">ms</span>
              </div>
              <p class="f-help">下单前等待的时间（可错开跟单时机）。</p>
            </div>
          </div>

          <h4 class="sub-section">订单备注</h4>
          <p class="f-help" style="margin: 0 0 10px;">
            写入跟单端订单的自定义备注（MT4/MT5 的 Comment、cTrader 的 Label）。留空 = 不加备注。
          </p>
          <div class="form-grid">
            <div class="field full">
              <label class="f-label" for="order-comment">备注内容</label>
              <input id="order-comment" type="text" placeholder="例如 Copy from &#123;symbol&#125; &#123;src_lot&#125;" bind:value={editing.order_comment} />
              <p class="f-help">
                支持占位符（可自由组合）：<code>&#123;symbol&#125;</code>（信号端品种）、<code>&#123;side&#125;</code>（买入/卖出）、<code>&#123;src_lot&#125;</code>（信号端原始手数）、<code>&#123;ticket&#125;</code>（订单编号）。
              </p>
            </div>
          </div>
          <label class="check-row">
            <input type="checkbox" bind:checked={editing.order_comment_src_lot} />
            <span class="check-text">
              <strong>备注包含信号端手数标记</strong>
              <span class="muted">在备注末尾附加 <code>[SRC x.xx]</code>（信号端原始手数）。</span>
            </span>
          </label>

          <h4 class="sub-section">报价偏差补偿</h4>
          <p class="f-help" style="margin: 0 0 10px;">
            按固定的点偏移调整特定品种的止损/止盈，即使跟单端经纪商报价出现偏差，
            止损也能落在预期价位。
          </p>
          {#if editing.quote_offsets.length === 0}
            <p class="f-help" style="margin: 0 0 10px;">暂无偏移 — 添加一条以补偿某品种。</p>
          {:else}
            <div class="qo-list">
              {#each editing.quote_offsets as o, i}
                <div class="qo-row">
                  <input type="text" class="qo-sym" placeholder="EURUSD"
                         value={o.symbol}
                         on:input={(e) => { editing.quote_offsets[i].symbol = e.currentTarget.value.toUpperCase(); editing.quote_offsets = editing.quote_offsets; }} />
                  <input type="text" class="qo-feed" placeholder="任意数据源"
                         title="可选的数据源 (例如 OANDA:、*PEPPERSTONE)。留空则匹配任意数据源 — 仅当信号端为 TradingView 且希望对不同数据源使用不同偏差值时才有意义。"
                         value={o.feed ?? ""}
                         on:input={(e) => { editing.quote_offsets[i].feed = e.currentTarget.value.toUpperCase(); editing.quote_offsets = editing.quote_offsets; }} />
                  <div class="input-suffix qo-pips">
                    <input type="number" step="0.1" placeholder="0.0"
                           bind:value={editing.quote_offsets[i].pips} />
                    <span class="suffix">pips</span>
                  </div>
                  <button type="button" class="icon-btn danger" title="移除"
                          on:click={() => editing.quote_offsets = editing.quote_offsets.filter((_, j) => j !== i)}>✕</button>
                </div>
              {/each}
            </div>
          {/if}
          <button type="button" class="ghost mt"
                  on:click={() => editing.quote_offsets = [...editing.quote_offsets, { symbol: "", pips: 0, feed: "" }]}>
            + 添加品种偏移
          </button>

        {:else if activeTab === "schedule"}
          <header class="sec-head">
            <h3 class="section-title">时段</h3>
            <p class="section-sub">将复制限制在每日时段内 (UTC)。</p>
          </header>

          <label class="check-row">
            <input type="checkbox" bind:checked={editing.schedule.enabled} />
            <span class="check-text">
              <strong>启用时段</strong>
              <span class="muted">在此时段之外，信号端交易将被跳过。</span>
            </span>
          </label>

          <div class="form-grid mt" class:dim={!editing.schedule.enabled}>
            <div class="field">
              <label class="f-label" for="start">开始时间 (UTC)</label>
              <input id="start" type="time"
                value={minToHHMM(editing.schedule.start_min)}
                on:input={(ev) => editing && (editing.schedule.start_min = hhmmToMin(ev.currentTarget.value))}
                disabled={!editing.schedule.enabled} />
            </div>
            <div class="field">
              <label class="f-label" for="end">结束时间 (UTC)</label>
              <input id="end" type="time"
                value={minToHHMM(editing.schedule.end_min)}
                on:input={(ev) => editing && (editing.schedule.end_min = hhmmToMin(ev.currentTarget.value))}
                disabled={!editing.schedule.enabled} />
            </div>
          </div>

          <label class="check-row mt">
            <input type="checkbox" bind:checked={editing.schedule.skip_weekends} />
            <span class="check-text">
              <strong>跳过周末</strong>
              <span class="muted">周六/周日 (UTC) 不复制。</span>
            </span>
          </label>

          <div class="hint-box">
            <span class="hint-icon">ℹ</span>
            <span>如果<em>结束</em>早于<em>开始</em>，时段将跨夜 (例如 22:00 → 06:00)。</span>
          </div>

        {:else if activeTab === "advanced"}
          <header class="sec-head">
            <h3 class="section-title">高级</h3>
            <p class="section-sub">需要实时报价数据源的持仓管理功能。</p>
          </header>

          <div class="form-grid">
            <div class="field">
              <label class="f-label" for="trail">移动止损</label>
              <div class="input-suffix">
                <input id="trail" type="number" min="0" step="0.1" bind:value={editing.trailing_pips} />
                <span class="suffix">pips</span>
              </div>
              <p class="f-help">0 = 禁用。</p>
            </div>
            <div class="field">
              <label class="f-label" for="be">保本触发点位</label>
              <div class="input-suffix">
                <input id="be" type="number" min="0" step="0.1" bind:value={editing.breakeven_after_pips} />
                <span class="suffix">pips</span>
              </div>
              <p class="f-help">当盈利达到该值时将止损移到开仓价。</p>
            </div>
          </div>
          <div class="hint-box warn">
            <span class="hint-icon">⚠</span>
            <span>
              移动止损与保本会保存在规则中，但引擎<strong>尚未执行</strong> —
              它们需要跟单端侧的报价。后续将实现。
            </span>
          </div>
        {/if}
      </section>
    </div>

    <footer class="drawer-foot">
      <div class="foot-preview">
        {#each chipsForRule(editing) as c}
          <span class="cfg-chip {c.kind}">{c.text}</span>
        {/each}
      </div>
      <div class="foot-actions">
        <button on:click={cancel}>取消</button>
        <button class="primary" on:click={save} disabled={!editing.master_id || !editing.slave_id}>
          {isEdit ? "保存更改" : "创建规则"}
        </button>
      </div>
    </footer>
  </aside>
{/if}

<style>
  .rules-root { overflow: hidden; }
  .header-left { display: flex; align-items: center; gap: 10px; }
  .count-pill {
    display: inline-flex; align-items: center; justify-content: center;
    min-width: 22px; height: 20px; padding: 0 7px;
    border-radius: 999px;
    background: var(--surface-muted); color: var(--text-2);
    font-size: 11px; font-weight: 600;
  }
  .btn-new { display: inline-flex; align-items: center; gap: 6px; }
  .btn-new .plus { font-size: 16px; line-height: 1; margin-top: -1px; }

  /* Empty state */
  .empty-state {
    padding: 56px 24px 64px;
    text-align: center;
    color: var(--text-2);
  }
  .empty-glyph {
    font-size: 36px; line-height: 1;
    width: 64px; height: 64px;
    border-radius: 16px;
    background: var(--primary-soft); color: var(--primary);
    display: inline-flex; align-items: center; justify-content: center;
    margin-bottom: 16px;
  }
  .empty-title { font-size: 15px; margin-bottom: 6px; color: var(--text); }
  .empty-sub { font-size: 13px; margin: 0 auto 18px; max-width: 380px; line-height: 1.55; }

  /* Rule cards */
  .rule-list { display: flex; flex-direction: column; }
  .rule {
    position: relative;
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 16px;
    align-items: center;
    padding: 16px 20px 16px 24px;
    border-bottom: 1px solid var(--border);
    transition: background 0.12s;
  }
  .rule:last-child { border-bottom: none; }
  .rule:hover { background: var(--surface-muted); }
  .rule.off { opacity: 0.7; }
  .rule.off .acc-label { color: var(--text-2); }

  .rule-status-bar {
    position: absolute; left: 0; top: 0; bottom: 0; width: 3px;
    background: #cbd5e1;
  }
  .rule-status-bar.on { background: var(--success); }
  .rule-status-bar.warn { background: #f59e0b; }
  .rule.warn { border-color: #fcd34d; background: var(--surface)beb; }
  .banner-warn {
    display: flex; align-items: flex-start; gap: 12px;
    margin: 12px 16px 0;
    padding: 12px 14px;
    background: var(--surface)beb; border: 1px solid #fcd34d;
    border-radius: var(--radius);
  }
  .banner-icon { font-size: 18px; line-height: 1; color: #b45309; }
  .banner-title { font-weight: 600; color: #92400e; font-size: 13px; }
  .banner-sub { color: #78350f; font-size: 12px; margin-top: 2px; }
  .warn-pill {
    display: inline-flex; align-items: center; gap: 4px;
    padding: 2px 8px; border-radius: 999px;
    background: #fef3c7; color: #92400e;
    font-size: 11px; font-weight: 600;
    border: 1px solid #fcd34d;
  }

  .rule-main { min-width: 0; display: flex; flex-direction: column; gap: 10px; }

  .rule-name-row { display: flex; align-items: center; gap: 8px; }
  .rule-name {
    margin: 0;
    font-size: 14px; font-weight: 600;
    color: var(--text); letter-spacing: -0.005em;
    line-height: 1.3;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
  }
  .rule-name.untitled { color: var(--text-muted); font-style: italic; font-weight: 500; }

  .rule-flow {
    display: flex; align-items: center; gap: 14px;
    flex-wrap: wrap;
  }
  .acc-block { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .role-tag {
    font-size: 9px; font-weight: 700; letter-spacing: 0.1em;
    color: var(--text-muted);
  }
  .role-tag.master { color: #4338ca; }
  .role-tag.slave { color: #047857; }
  .acc-line { display: inline-flex; align-items: center; gap: 8px; min-width: 0; }
  .acc-label { font-weight: 600; color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 200px; }

  .flow-arrow {
    display: inline-flex; align-items: center; gap: 0;
    color: var(--text-muted);
    margin-bottom: -6px;
  }
  .arrow-line {
    display: inline-block; width: 28px; height: 1.5px;
    background: linear-gradient(to right, transparent, var(--border-strong));
  }
  .arrow-head { font-size: 9px; transform: translateX(-2px); color: var(--border-strong); }

  .rule-chips { display: flex; flex-wrap: wrap; gap: 5px; }
  .cfg-chip {
    display: inline-flex; align-items: center;
    padding: 2px 8px;
    border-radius: 5px;
    font-size: 11px; font-weight: 500;
    background: var(--surface-muted); color: var(--text-2);
    border: 1px solid transparent;
  }
  .cfg-chip.primary { background: var(--primary-soft); color: var(--primary); }
  .cfg-chip.info    { background: var(--surface-muted); color: var(--text-2); }
  .cfg-chip.warn    { background: var(--surface)beb; color: #b45309; }
  .cfg-chip.danger  { background: #fef2f2; color: #b91c1c; }

  .rule-actions { display: inline-flex; align-items: center; gap: 6px; }

  .icon-btn {
    width: 32px; height: 32px; padding: 0;
    display: inline-flex; align-items: center; justify-content: center;
    border: 1px solid transparent;
    border-radius: 8px;
    background: transparent;
    color: var(--text-2);
    font-size: 13px;
    cursor: pointer;
  }
  .icon-btn:hover { background: var(--surface-muted); color: var(--text); }
  .icon-btn.danger:hover { background: #fef2f2; color: var(--danger); }
  .icon-btn.close { color: var(--text-2); }

  /* Toggle */
  .toggle {
    display: inline-flex; align-items: center; gap: 8px;
    padding: 4px 10px 4px 4px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--surface);
    font-size: 12px; font-weight: 600;
    color: #64748b;
    cursor: pointer;
  }
  .toggle-track {
    position: relative; display: inline-block;
    width: 28px; height: 16px;
    background: #cbd5e1; border-radius: 999px;
    transition: background 0.15s ease;
  }
  .toggle-thumb {
    position: absolute; top: 2px; left: 2px;
    width: 12px; height: 12px;
    background: var(--surface); border-radius: 50%;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
    transition: left 0.15s ease;
  }
  .toggle.on { border-color: #10b981; background: #ecfdf5; color: #047857; }
  .toggle.on .toggle-track { background: #10b981; }
  .toggle.on .toggle-thumb { left: 14px; }

  /* 补单：与运行中同款的胶囊按钮，红色主题 */
  .resync-btn {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 4px 10px;
    border: 1px solid var(--danger);
    border-radius: 999px;
    background: transparent;
    font-size: 12px; font-weight: 600;
    color: var(--danger);
    cursor: pointer;
    transition: background 0.12s ease;
  }
  .resync-btn:hover { background: rgba(239, 68, 68, 0.1); }
  .resync-btn.busy, .resync-btn:disabled { opacity: 0.6; cursor: progress; }
  .resync-dot {
    width: 8px; height: 8px; border-radius: 50%;
    background: currentColor;
  }

  /* 编辑：与补单同款胶囊，主色（蓝）主题 */
  .edit-btn {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 4px 10px;
    border: 1px solid var(--primary);
    border-radius: 999px;
    background: transparent;
    font-size: 12px; font-weight: 600;
    color: var(--primary);
    cursor: pointer;
    transition: background 0.12s ease;
  }
  .edit-btn:hover { background: var(--primary-soft); }

  /* 清仓：与补单同款胶囊，但用实心橙色填充 — 与红色描边的补单明显区分 */
  .close-btn {
    display: inline-flex; align-items: center; gap: 6px;
    padding: 4px 10px;
    border: 1px solid #f59e0b;
    border-radius: 999px;
    background: #f59e0b;
    font-size: 12px; font-weight: 600;
    color: #7c2d12;
    cursor: pointer;
    transition: background 0.12s ease, border-color 0.12s ease;
  }
  .close-btn:hover { background: #fbbf24; border-color: #fbbf24; }
  .close-btn.busy, .close-btn:disabled { opacity: 0.6; cursor: progress; }
  .close-dot {
    width: 8px; height: 8px; border-radius: 50%;
    background: currentColor;
  }

  /* Drawer */
  .overlay {
    position: fixed; inset: 0; z-index: 40;
    background: rgba(15, 23, 42, 0.32);
    backdrop-filter: blur(2px);
  }
  .drawer {
    position: fixed; top: 0; right: 0; bottom: 0;
    width: min(760px, 100vw);
    z-index: 50;
    background: var(--surface);
    border-left: 1px solid var(--border);
    box-shadow: -10px 0 30px rgba(15, 23, 42, 0.12);
    display: flex; flex-direction: column;
  }
  .drawer-head {
    display: flex; align-items: flex-start; justify-content: space-between;
    padding: 18px 28px 14px;
    border-bottom: 1px solid var(--border);
  }
  .drawer-eyebrow {
    font-size: 10px; font-weight: 700; letter-spacing: 0.1em;
    color: var(--primary); text-transform: uppercase;
    margin-bottom: 4px;
  }
  .drawer-head-main { flex: 1; min-width: 0; }
  .drawer-name {
    width: 100%;
    margin-top: 2px;
    padding: 4px 8px; margin-left: -8px;
    font-family: inherit;
    font-size: 18px; font-weight: 600;
    color: var(--text); letter-spacing: -0.01em;
    background: transparent; border: 1px solid transparent;
    border-radius: 6px;
    transition: background 0.12s, border-color 0.12s;
    height: auto;
  }
  .drawer-name:hover { background: var(--surface-muted); }
  .drawer-name:focus {
    background: var(--surface);
    border-color: var(--primary);
    box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.12);
    outline: none;
  }
  .drawer-name::placeholder { color: var(--text-muted); font-weight: 500; }

  .drawer-pair {
    display: grid;
    grid-template-columns: 1fr auto 1fr auto;
    align-items: end; gap: 12px;
    padding: 14px 28px;
    background: var(--surface-muted);
    border-bottom: 1px solid var(--border);
  }
  .pair-block { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .pair-label { font-size: 10px; font-weight: 700; letter-spacing: 0.1em; color: var(--text-muted); text-transform: uppercase; }
  .pair-select { width: 100%; }
  .pair-arrow { color: var(--text-muted); padding-bottom: 9px; font-size: 14px; }
  .pair-toggle {
    display: inline-flex; align-items: center; gap: 8px;
    padding: 7px 12px 7px 6px;
    border: 1px solid var(--border);
    border-radius: 999px; background: var(--surface);
    font-size: 12px; font-weight: 600;
    color: #64748b; cursor: pointer;
    align-self: end;
  }
  .pair-toggle.on { border-color: var(--success); background: #ecfdf5; color: #047857; }
  .pair-toggle.on .toggle-track { background: var(--success); }
  .pair-toggle.on .toggle-thumb { left: 14px; }
  .pair-toggle input { display: none; }

  .drawer-body {
    flex: 1; min-height: 0;
    display: grid;
    grid-template-columns: 200px 1fr;
  }

  /* Vertical tabs */
  .vtabs {
    border-right: 1px solid var(--border);
    background: var(--surface-muted);
    padding: 12px 8px;
    overflow-y: auto;
  }
  .vtab {
    display: flex; align-items: flex-start; gap: 10px;
    width: 100%; text-align: left;
    padding: 9px 10px;
    border: 1px solid transparent;
    border-radius: 8px; background: transparent;
    color: var(--text-2);
    margin-bottom: 2px; cursor: pointer;
    transition: background 0.12s, color 0.12s, border-color 0.12s;
  }
  .vtab:hover { background: var(--surface); color: var(--text); }
  .vtab.on {
    background: var(--surface); color: var(--primary);
    border-color: var(--border); box-shadow: var(--shadow-sm);
  }
  .vtab-icon {
    width: 24px; height: 24px;
    display: inline-flex; align-items: center; justify-content: center;
    border-radius: 6px;
    background: var(--surface-muted);
    font-size: 12px; flex: none;
  }
  .vtab.on .vtab-icon { background: var(--primary-soft); color: var(--primary); }
  .vtab-text { display: flex; flex-direction: column; min-width: 0; }
  .vtab-label { font-size: 13px; font-weight: 600; }
  .vtab-desc  { font-size: 11px; color: var(--text-muted); font-weight: 400; line-height: 1.3; }
  .vtab.on .vtab-desc { color: var(--text-2); }

  /* Tab body */
  .vbody { padding: 24px 32px 32px; overflow-y: auto; }
  .sec-head { margin-bottom: 20px; }
  .section-title {
    font-size: 15px; color: var(--text); font-weight: 600;
    text-transform: none; letter-spacing: -0.005em;
    margin: 0 0 4px;
  }
  .section-sub { font-size: 12.5px; color: var(--text-2); margin: 0; line-height: 1.5; }
  .sub-section {
    font-size: 11px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.07em;
    color: var(--text-muted); margin: 24px 0 10px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--border);
  }

  /* Form grid — predictable 2-col, collapses to 1 below ~480px container */
  .form-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    gap: 16px 18px;
    align-items: start;
  }
  .form-grid.mt { margin-top: 14px; }
  .mt { margin-top: 10px; }
  .qo-list { display: flex; flex-direction: column; gap: 6px; margin-bottom: 8px; }
  .qo-row { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 0.8fr) 130px 32px; gap: 8px; align-items: center; }
  .qo-row > * { min-width: 0; }
  .qo-sym { text-transform: uppercase; width: 100%; }
  .qo-feed { text-transform: uppercase; width: 100%; }
  .qo-pips { width: 100%; }
  .qo-pips input { width: 100%; }
  .qo-row .icon-btn { width: 32px; height: 32px; padding: 0; }
  .ghost {
    display: inline-flex; align-items: center; gap: 4px;
    padding: 6px 10px;
    background: transparent;
    border: 1px dashed var(--border);
    border-radius: var(--radius-sm);
    color: var(--text-2);
    font-size: 12px; cursor: pointer;
  }
  .ghost:hover { background: var(--surface-muted); color: var(--text); border-style: solid; }
  .form-grid.dim { opacity: 0.55; pointer-events: none; }

  /* Field: label / control / helper, all left-aligned */
  .field {
    min-width: 0;
    display: flex; flex-direction: column;
    gap: 6px;
  }
  .field.full { grid-column: 1 / -1; }

  .f-label {
    font-size: 12.5px; font-weight: 500;
    color: var(--text);
    line-height: 1.3;
    text-transform: none; letter-spacing: 0;
  }
  .f-help {
    margin: 0;
    font-size: 11.5px; color: var(--text-muted);
    line-height: 1.4;
  }

  .field input,
  .field select {
    width: 100%; min-width: 0;
    height: 36px;
    padding: 0 12px;
    box-sizing: border-box;
    font-size: 13px;
  }
  .field input[type="time"] { padding: 0 10px; }
  .field input[type="number"] { -moz-appearance: textfield; appearance: textfield; }
  .field input[type="number"]::-webkit-outer-spin-button,
  .field input[type="number"]::-webkit-inner-spin-button {
    -webkit-appearance: none; margin: 0;
  }

  /* Input with unit suffix */
  .input-suffix {
    position: relative;
    display: flex; align-items: stretch;
  }
  .input-suffix input {
    padding-right: 56px;
    font-variant-numeric: tabular-nums;
  }
  .input-suffix .suffix {
    position: absolute; right: 0; top: 0; bottom: 0;
    display: inline-flex; align-items: center; justify-content: center;
    padding: 0 12px;
    border-left: 1px solid var(--border);
    background: var(--surface-muted);
    color: var(--text-muted);
    font-size: 11px; font-weight: 600;
    letter-spacing: 0.02em;
    pointer-events: none;
    border-radius: 0 var(--radius) var(--radius) 0;
    min-width: 44px;
  }
  .input-suffix input:focus + .suffix {
    border-left-color: var(--primary);
    color: var(--primary);
  }

  /* Radio cards (lot mode) */
  .radio-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(170px, 1fr));
    gap: 8px;
  }
  .radio-card {
    display: flex; align-items: center; gap: 10px;
    padding: 12px;
    border: 1.5px solid var(--border); border-radius: 10px;
    background: var(--surface); cursor: pointer;
    transition: border-color 0.12s, background 0.12s, transform 0.08s;
  }
  .radio-card:hover { border-color: var(--border-strong); }
  .radio-card.on {
    border-color: var(--primary);
    background: var(--primary-soft);
  }
  .radio-card input { display: none; }
  .rc-icon {
    width: 32px; height: 32px;
    display: inline-flex; align-items: center; justify-content: center;
    border-radius: 8px;
    background: var(--surface-muted); color: var(--text-2);
    font-size: 14px; font-weight: 700;
    flex: none;
  }
  .radio-card.on .rc-icon { background: var(--surface); color: var(--primary); }
  .rc-text { display: flex; flex-direction: column; min-width: 0; }
  .rc-title { font-size: 13px; font-weight: 600; color: var(--text); }
  .rc-hint  { font-size: 11px; color: var(--text-muted); }

  /* Check rows */
  .check-row {
    display: flex; align-items: flex-start; gap: 12px;
    padding: 12px 14px;
    border: 1px solid var(--border); border-radius: 8px;
    background: var(--surface);
    cursor: pointer;
    font-size: 13px;
    transition: border-color 0.12s, background 0.12s;
  }
  .check-row:hover { border-color: var(--border-strong); background: var(--surface-muted); }
  .check-row.mt { margin-top: 14px; }
  .check-row input { width: 16px; height: 16px; flex: none; cursor: pointer; margin-top: 1px; }
  .check-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .check-text strong { font-weight: 600; color: var(--text); }
  .check-text .muted { font-weight: 400; font-size: 12px; line-height: 1.4; }

  /* Hint boxes */
  .hint-box {
    display: flex; align-items: flex-start; gap: 10px;
    margin-top: 14px;
    padding: 10px 12px;
    border-radius: 8px;
    background: var(--surface-muted);
    border: 1px solid var(--border);
    font-size: 12px; color: var(--text-2);
    line-height: 1.5;
  }
  .hint-box code { background: var(--surface); padding: 1px 6px; border-radius: 4px; font-size: 11px; }
  .hint-icon {
    width: 18px; height: 18px;
    display: inline-flex; align-items: center; justify-content: center;
    border-radius: 50%;
    background: var(--surface); color: var(--primary);
    font-size: 11px; font-weight: 700;
    flex: none;
  }
  .hint-box.warn { background: var(--surface)beb; border-color: #fde68a; color: #92400e; }
  .hint-box.warn .hint-icon { color: #b45309; }

  /* Symbol map editor */
  .map-list { display: flex; flex-direction: column; gap: 6px; margin: 8px 0 10px; }
  .map-row {
    display: grid;
    grid-template-columns: 1fr auto 1fr auto;
    align-items: center;
    gap: 8px;
  }
  .map-row input {
    font: inherit; font-size: 13px;
    padding: 7px 10px;
    border: 1px solid var(--border); border-radius: 6px;
    background: var(--surface); min-width: 0;
    font-variant-numeric: tabular-nums;
  }
  .map-row input:focus { outline: none; border-color: var(--primary); box-shadow: 0 0 0 3px var(--primary-soft); }
  .map-arrow { color: var(--text-muted); font-weight: 600; }
  .map-remove {
    width: 28px; height: 28px;
    border: 1px solid var(--border); border-radius: 6px;
    background: var(--surface); color: var(--text-muted);
    font-size: 12px; cursor: pointer;
  }
  .map-remove:hover { background: #fef2f2; border-color: #fecaca; color: #b91c1c; }
  .map-add {
    align-self: flex-start;
    padding: 6px 12px;
    border: 1px dashed var(--border); border-radius: 6px;
    background: transparent; color: var(--text-2);
    font-size: 12px; font-weight: 500; cursor: pointer;
  }
  .map-add:hover { border-color: var(--primary); color: var(--primary); background: var(--primary-soft); }

  /* Drawer footer */
  .drawer-foot {
    display: flex; align-items: center; justify-content: space-between;
    gap: 12px;
    padding: 14px 28px;
    border-top: 1px solid var(--border);
    background: var(--surface);
  }
  .foot-preview {
    display: flex; flex-wrap: wrap; gap: 4px;
    flex: 1; min-width: 0;
    max-height: 48px; overflow: hidden;
  }
  .foot-actions { display: flex; gap: 8px; flex: none; }

  @media (max-width: 860px) {
    .drawer-body { grid-template-columns: 168px 1fr; }
    .vtab-desc { display: none; }
  }
  @media (max-width: 680px) {
    .drawer-body { grid-template-columns: 1fr; }
    .vtabs {
      display: flex; gap: 4px; padding: 8px;
      overflow-x: auto;
      border-right: none; border-bottom: 1px solid var(--border);
    }
    .vtab { white-space: nowrap; margin-bottom: 0; flex: none; }
    .vtab-text { display: none; }
    .vbody { padding: 18px 18px 22px; }
    .drawer-head, .drawer-pair, .drawer-foot { padding-left: 18px; padding-right: 18px; }
    .drawer-pair {
      grid-template-columns: 1fr 1fr;
      row-gap: 10px;
    }
    .pair-arrow { display: none; }
    .pair-toggle { grid-column: 1 / -1; justify-content: center; }
  }
</style>
