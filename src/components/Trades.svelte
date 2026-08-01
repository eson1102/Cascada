<script lang="ts">
  import type { Account, CopyRule, Trade } from "../lib/api";
  import { accountIndex, formatTime, labelOf } from "../lib/format";
  import VirtualList from "../lib/VirtualList.svelte";
  export let trades: Trade[];
  export let accounts: Account[];
  export let rules: CopyRule[];
  $: idx = accountIndex(accounts);

  // 只显示跟单端（slave 账户）的订单
  $: slaveOnly = trades.filter((t) => {
    const acc = idx.get(t.account_id);
    return acc?.role === "Slave";
  });

  // 按规则筛选："" = 全部；规则 id = 只看该规则下属订单
  let ruleFilter = "";
  $: ruleName = (id: string) => rules.find((r) => r.id === id)?.name?.trim() || "未命名规则";
  $: filtered = ruleFilter
    ? slaveOnly.filter((t) => t.rule_id === ruleFilter)
    : slaveOnly;
  $: status = (t: Trade) => (t.profit != null ? "已平仓" : "持仓中");
  $: statusCls = (t: Trade) => (t.profit != null ? "done" : "open");
</script>

<div class="card">
  <div class="card-header">
    <h2>交易</h2>
    <div class="head-right">
      <select class="rule-filter" bind:value={ruleFilter} title="按跟单规则筛选">
        <option value="">全部跟单端（{slaveOnly.length}）</option>
        {#each rules as r (r.id)}
          {@const n = slaveOnly.filter((t) => t.rule_id === r.id).length}
          {#if n > 0}
            <option value={r.id}>{r.name?.trim() || "未命名规则"}（{n}）</option>
          {/if}
        {/each}
      </select>
      <span class="chip">显示 {filtered.length} 条</span>
    </div>
  </div>
  {#if filtered.length === 0}
    <div class="empty">暂无跟单交易{ruleFilter ? "（该规则下）" : ""}。</div>
  {:else}
    <div class="tbl">
      <div class="row head">
        <span>时间</span><span>账户</span><span>单号</span><span>品种</span><span>方向</span>
        <span>状态</span>
        <span class="right">手数</span><span class="right">价格</span>
        <span class="right">盈亏</span><span>魔术号</span>
      </div>
      <div class="vl-wrap">
        <VirtualList items={filtered} rowHeight={32}>
          <svelte:fragment let:item={t}>
            <div class="row">
              <span class="num muted">{formatTime(t.opened_at)}</span>
              <span class="strong">{labelOf(idx, t.account_id)}</span>
              <span class="num muted">{t.ticket}</span>
              <span>{t.symbol}</span>
              <span><span class="chip" class:success={t.side === "Buy"} class:danger={t.side === "Sell"}>{t.side === "Buy" ? "买入" : t.side === "Sell" ? "卖出" : t.side}</span></span>
              <span><span class="chip st" class:open={statusCls(t) === "open"} class:done={statusCls(t) === "done"}>{status(t)}</span></span>
              <span class="right num">{t.volume}</span>
              <span class="right num">{t.price}</span>
              <span class="right num" class:pos={(t.profit ?? 0) > 0} class:neg={(t.profit ?? 0) < 0}>
                {t.profit != null ? t.profit.toFixed(2) : "—"}
              </span>
              <span class="right num muted">{t.magic ? t.magic : "—"}</span>
            </div>
          </svelte:fragment>
        </VirtualList>
      </div>
    </div>
  {/if}
</div>

<style>
  .card-header { display: flex; align-items: center; justify-content: space-between; gap: 10px; flex-wrap: wrap; }
  .head-right { display: flex; align-items: center; gap: 8px; }
  .rule-filter {
    padding: 5px 8px; font-size: 12px;
    background: var(--surface-muted); color: var(--text);
    border: 1px solid var(--border); border-radius: 8px;
  }
  .tbl { display: flex; flex-direction: column; height: calc(100vh - 200px); min-height: 300px; }
  .row {
    display: grid;
    grid-template-columns: 105px 1fr 100px 84px 64px 70px 64px 84px 80px 70px;
    align-items: center;
    height: 32px;
    padding: 0 8px;
    border-bottom: 1px solid var(--border);
    font-size: 13px;
  }
  .row.head { font-weight: 600; color: var(--text-2); background: var(--surface-muted); border-bottom: 1px solid var(--border); }
  .right { text-align: right; }
  .vl-wrap { flex: 1; min-height: 0; }
  .chip.st { font-size: 10px; font-weight: 600; padding: 1px 7px; }
  .chip.st.open { background: #ecfdf5; color: #047857; }
  .chip.st.done { background: var(--surface-muted); color: var(--text-2); }
  .pos { color: #047857; }
  .neg { color: #dc2626; }
</style>
