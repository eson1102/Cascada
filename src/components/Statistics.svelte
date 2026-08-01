<script lang="ts">
  import type { Account, CopyRule, Trade } from "../lib/api";

  export let trades: Trade[];
  export let rules: CopyRule[];
  export let accounts: Account[];

  // 规则筛选："all" | "none"(未关联) | 具体 rule id
  let selectedRule: string = "all";
  $: ruleName = (id: string): string => {
    const r = rules.find((x) => x.id === id);
    return r ? (r.name?.trim() || "未命名规则") : "已删除的规则";
  };

  // ---- 防抖快照 ----
  // App 层在交易/心跳活跃时会高频替换 trades 数组（rAF flush）。
  // 直接驱动全部 reactive 会让统计页在历史重放时每帧全量重算+重渲染
  // 明细表，主线程被阻塞 → 页面假死。改为等待 150ms 静默后再快照。
  let snapshot: Trade[] = [];
  let debounceTimer: ReturnType<typeof setTimeout> | undefined;
  $: sync(trades);
  function sync(t: Trade[]) {
    if (t === snapshot) return;
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      snapshot = t;
      debounceTimer = undefined;
    }, 150);
  }

  $: filtered = snapshot.filter((t) => {
    if (selectedRule === "all") return true;
    if (selectedRule === "none") return !t.rule_id;
    return t.rule_id === selectedRule;
  });

  // ---- 汇总 ----
  $: summary = (() => {
    const closed = filtered.filter((t) => t.profit != null);
    const wins = closed.filter((t) => (t.profit ?? 0) > 0);
    const losses = closed.filter((t) => (t.profit ?? 0) < 0);
    const totalPnl = closed.reduce((s, t) => s + (t.profit ?? 0), 0);
    const totalLots = filtered.reduce((s, t) => s + t.volume, 0);
    return {
      orders: filtered.length,
      closed: closed.length,
      wins: wins.length,
      losses: losses.length,
      totalPnl,
      totalLots,
      winRate: closed.length ? (wins.length / closed.length) * 100 : 0,
    };
  })();

  // ---- 日历 ----
  const now = new Date();
  let year = now.getFullYear();
  let month = now.getMonth();

  $: monthTitle = `${year} 年 ${month + 1} 月`;
  $: calendar = buildCalendar();
  $: monthDetail = filtered
    .filter((t) => {
      const d = new Date(t.opened_at);
      return d.getFullYear() === year && d.getMonth() === month;
    })
    .sort((a, b) => b.opened_at - a.opened_at);

  function buildCalendar(): (null | { day: number; orders: number; pnl: number })[] {
    const first = new Date(year, month, 1);
    const daysInMonth = new Date(year, month + 1, 0).getDate();
    const map = new Map<number, { orders: number; pnl: number }>();
    for (const t of filtered) {
      const d = new Date(t.opened_at);
      if (d.getFullYear() === year && d.getMonth() === month) {
        const e = map.get(d.getDate()) || { orders: 0, pnl: 0 };
        e.orders++;
        if (t.profit != null) e.pnl += t.profit;
        map.set(d.getDate(), e);
      }
    }
    const cells: (null | { day: number; orders: number; pnl: number })[] = [];
    for (let i = 0; i < first.getDay(); i++) cells.push(null);
    for (let d = 1; d <= daysInMonth; d++) {
      const e = map.get(d);
      cells.push(e ? { day: d, orders: e.orders, pnl: e.pnl } : { day: d, orders: 0, pnl: 0 });
    }
    return cells;
  }

  function prevMonth() {
    if (month === 0) { month = 11; year--; } else { month--; }
  }
  function nextMonth() {
    if (month === 11) { month = 0; year++; } else { month++; }
  }
  function today() {
    year = now.getFullYear();
    month = now.getMonth();
  }

  function fmtPnl(v: number): string {
    return (v >= 0 ? "+" : "") + v.toFixed(2);
  }
  function pnlClass(v: number): string {
    return v > 0 ? "pos" : v < 0 ? "neg" : "";
  }
  function fmtTime(ms: number): string {
    const d = new Date(ms);
    return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  }
</script>

<div class="stats">
  <header class="sec-head">
    <h3 class="section-title">跟单统计</h3>
    <select class="rule-filter" bind:value={selectedRule}>
      <option value="all">全部规则</option>
      <option value="none">未关联规则</option>
      {#each rules as r}
        <option value={r.id}>{r.name?.trim() || "未命名规则"}</option>
      {/each}
    </select>
  </header>

  <!-- 汇总卡片 -->
  <div class="cards">
    <div class="card">
      <span class="card-label">订单数量</span>
      <span class="card-value">{summary.orders}</span>
      <span class="card-sub">总手数 {summary.totalLots.toFixed(2)}</span>
    </div>
    <div class="card">
      <span class="card-label">已平仓</span>
      <span class="card-value">{summary.closed}</span>
      <span class="card-sub">盈利 {summary.wins} / 亏损 {summary.losses}</span>
    </div>
    <div class="card">
      <span class="card-label">总盈亏</span>
      <span class="card-value {pnlClass(summary.totalPnl)}">{fmtPnl(summary.totalPnl)}</span>
      <span class="card-sub">已实现盈亏（不含在途持仓）</span>
    </div>
    <div class="card">
      <span class="card-label">胜率</span>
      <span class="card-value">{summary.winRate.toFixed(1)}%</span>
      <span class="card-sub">按已平仓订单计算</span>
    </div>
  </div>

  <!-- 日历 -->
  <div class="panel">
    <div class="cal-head">
      <button class="cal-nav" on:click={prevMonth} title="上一月">‹</button>
      <span class="cal-title">{monthTitle}</span>
      <button class="cal-nav" on:click={nextMonth} title="下一月">›</button>
      <button class="cal-today" on:click={today} title="回到本月">本月</button>
    </div>
    <div class="cal-grid">
      <span class="cal-dow">日</span><span class="cal-dow">一</span><span class="cal-dow">二</span>
      <span class="cal-dow">三</span><span class="cal-dow">四</span><span class="cal-dow">五</span>
      <span class="cal-dow">六</span>
      {#each calendar as cell}
        {#if cell === null}
          <span class="cal-cell empty"></span>
        {:else}
          <span class="cal-cell {cell.orders ? 'has-data' : ''} {pnlClass(cell.pnl)}"
                title={cell.orders ? `${cell.day} 日：${cell.orders} 笔，盈亏 ${fmtPnl(cell.pnl)}` : `${cell.day} 日：无交易`}>
            <span class="cal-day">{cell.day}</span>
            {#if cell.orders}
              <span class="cal-orders">{cell.orders} 笔</span>
              <span class="cal-pnl">{fmtPnl(cell.pnl)}</span>
            {/if}
          </span>
        {/if}
      {/each}
    </div>
    <div class="cal-legend">
      <span><i class="legend-dot pos"></i>盈利</span>
      <span><i class="legend-dot neg"></i>亏损</span>
      <span><i class="legend-dot plain"></i>无交易</span>
    </div>
  </div>

  <!-- 当月明细 -->
  <div class="panel">
    <h4 class="panel-title">{monthTitle} 订单明细</h4>
    {#if monthDetail.length === 0}
      <p class="muted empty-tip">该月没有符合条件的订单。</p>
    {:else}
      {@const shown = monthDetail.slice(0, 100)}
      <table class="tbl">
        <thead>
          <tr>
            <th>时间</th><th>规则</th><th>品种</th><th>方向</th>
            <th>手数</th><th>信号端</th><th>盈亏</th>
          </tr>
        </thead>
        <tbody>
          {#each shown as t}
            <tr>
              <td class="muted">{fmtTime(t.opened_at)}</td>
              <td>{t.rule_id ? ruleName(t.rule_id) : "—"}</td>
              <td>{t.symbol}</td>
              <td class="side {t.side === "Buy" ? "buy" : "sell"}">{t.side === "Buy" ? "买" : "卖"}</td>
              <td>{t.volume.toFixed(2)}</td>
              <td class="muted">{t.origin_ticket ? `#${t.origin_ticket}` : "—"}</td>
              <td class={pnlClass(t.profit ?? 0)}>{t.profit != null ? fmtPnl(t.profit) : "在途"}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      {#if monthDetail.length > 100}
        <p class="muted empty-tip">仅显示最近 100 条（当月共 {monthDetail.length} 条）</p>
      {/if}
    {/if}
  </div>
</div>

<style>
  .stats { padding: 18px 20px; display: flex; flex-direction: column; gap: 16px; }
  .sec-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .section-title { margin: 0; font-size: 15px; font-weight: 700; }
  .rule-filter {
    padding: 6px 10px; font-size: 12px;
    background: var(--surface); color: var(--text);
    border: 1px solid var(--border); border-radius: var(--radius-sm);
  }

  .cards { display: grid; grid-template-columns: repeat(4, 1fr); gap: 12px; }
  .card {
    display: flex; flex-direction: column; gap: 4px;
    padding: 14px 16px;
    background: var(--surface);
    border: 1px solid var(--border); border-radius: var(--radius);
  }
  .card-label { font-size: 11px; color: var(--text-muted); font-weight: 600; }
  .card-value { font-size: 22px; font-weight: 700; color: var(--text); }
  .card-sub { font-size: 11px; color: var(--text-muted); }

  .panel {
    background: var(--surface);
    border: 1px solid var(--border); border-radius: var(--radius);
    padding: 14px 16px;
  }
  .panel-title { margin: 0 0 10px; font-size: 13px; font-weight: 700; }
  .empty-tip { font-size: 12px; }

  /* 日历 */
  .cal-head { display: flex; align-items: center; gap: 8px; margin-bottom: 10px; }
  .cal-title { font-size: 14px; font-weight: 700; min-width: 110px; text-align: center; }
  .cal-nav {
    width: 26px; height: 26px; border: 1px solid var(--border);
    background: var(--surface-muted); color: var(--text-2);
    border-radius: var(--radius-sm); cursor: pointer; font-size: 14px; line-height: 1;
  }
  .cal-nav:hover { color: var(--primary); border-color: var(--primary); }
  .cal-today {
    margin-left: 6px; padding: 4px 10px; font-size: 11px;
    border: 1px solid var(--border); background: var(--surface-muted);
    color: var(--text-2); border-radius: var(--radius-sm); cursor: pointer;
  }
  .cal-today:hover { color: var(--primary); border-color: var(--primary); }

  .cal-grid {
    display: grid; grid-template-columns: repeat(7, 1fr);
    gap: 4px;
  }
  .cal-dow {
    text-align: center; font-size: 11px; font-weight: 600;
    color: var(--text-muted); padding: 4px 0;
  }
  .cal-cell {
    min-height: 58px;
    display: flex; flex-direction: column; align-items: center; justify-content: flex-start;
    gap: 2px; padding: 5px 4px;
    border: 1px solid var(--border); border-radius: 6px;
    background: var(--surface-muted);
    font-size: 11px; color: var(--text-2);
  }
  .cal-cell.empty { border-color: transparent; background: transparent; }
  .cal-cell.has-data.pos { background: rgba(220, 38, 38, 0.08); border-color: rgba(220, 38, 38, 0.35); }
  .cal-cell.has-data.neg { background: rgba(22, 163, 74, 0.08); border-color: rgba(22, 163, 74, 0.35); }
  .cal-day { font-weight: 700; color: var(--text); align-self: flex-start; }
  .cal-orders { color: var(--text-2); }
  .cal-pnl { font-weight: 600; color: var(--text); }
  .cal-pnl.pos { color: #dc2626; }
  .cal-pnl.neg { color: #16a34a; }

  .cal-legend { display: flex; gap: 14px; margin-top: 10px; font-size: 11px; color: var(--text-muted); }
  .cal-legend span { display: inline-flex; align-items: center; gap: 5px; }
  .legend-dot { width: 9px; height: 9px; border-radius: 2px; display: inline-block; }
  .legend-dot.pos { background: rgba(220, 38, 38, 0.5); }
  .legend-dot.neg { background: rgba(22, 163, 74, 0.5); }
  .legend-dot.plain { background: var(--surface-muted); border: 1px solid var(--border); }

  /* 明细表 */
  .tbl { width: 100%; border-collapse: collapse; font-size: 12px; }
  .tbl th {
    text-align: left; padding: 6px 8px; font-size: 11px; font-weight: 600;
    color: var(--text-muted); border-bottom: 1px solid var(--border);
  }
  .tbl td { padding: 6px 8px; border-bottom: 1px solid var(--border); color: var(--text); }
  .tbl tr:last-child td { border-bottom: none; }
  .side.buy { color: #dc2626; }
  .side.sell { color: #16a34a; }
  .pos { color: #dc2626; }
  .neg { color: #16a34a; }
  .muted { color: var(--text-muted); }
</style>
