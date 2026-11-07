<template>
  <div class="result-panel">
    <div class="result-panel__header">
      <div class="tq-tabs">
        <button class="tq-tab" :class="{ active: queryStore.resultViewMode === 'table' }" @click="queryStore.resultViewMode = 'table'">Table</button>
        <button class="tq-tab" :class="{ active: queryStore.resultViewMode === 'json' }" @click="queryStore.resultViewMode = 'json'">JSON</button>
      </div>

      <!-- Stats bar -->
      <div class="result-panel__stats" v-if="queryStore.results">
        <span><strong>{{ queryStore.results.found?.toLocaleString() ?? 0 }}</strong> found</span>
        <span class="result-panel__sep" />
        <span><strong>{{ queryStore.results.out_of?.toLocaleString() ?? 0 }}</strong> total</span>
        <span class="result-panel__sep" />
        <span class="result-panel__timing-wrap">
          <span
            :key="fastAnimKey"
            class="result-panel__timing-toggle"
            :class="{ 'result-panel__timing-toggle--fast': isFast }"
            @click.stop="showTimingDetail = !showTimingDetail"
          >{{ formatServerTime(queryStore.results) }} &#9662;</span>
          <div v-if="showTimingDetail" class="result-panel__timing-popover" @click.stop>
            <div class="result-panel__timing-row">
              <span class="result-panel__timing-value">{{ formatServerTime(queryStore.results) }}</span>
              <span class="result-panel__timing-label">server</span>
            </div>
            <div class="result-panel__timing-divider" />
            <div class="result-panel__timing-row">
              <span class="result-panel__timing-value">{{ formatTimeMs(queryStore.timeMs) }}</span>
              <span class="result-panel__timing-label">round-trip</span>
            </div>
          </div>
        </span>
        <span class="result-panel__sep" />
        <span>page <strong>{{ queryStore.results.page ?? 1 }}</strong></span>
      </div>
    </div>

    <div v-if="queryStore.error" class="result-panel__error">
      {{ queryStore.error }}
    </div>

    <div v-else-if="queryStore.isExecuting" class="tq-empty">
      <div class="tq-spinner" />
      <span>Searching...</span>
    </div>

    <div v-else-if="!queryStore.results" class="tq-empty">
      <span>Run a query to see results</span>
      <span style="font-size: 11px; color: var(--tq-text-muted);">Ctrl+Enter to execute</span>
    </div>

    <template v-else>
      <div class="result-panel__body" v-if="queryStore.resultViewMode === 'table'">
        <div class="result-panel__content">
          <TableView :hits="queryStore.hits" />
        </div>
        <FacetPanel v-if="queryStore.results.facet_counts?.length" :facets="queryStore.results.facet_counts" />
      </div>
      <div class="result-panel__body" v-else>
        <JsonView :data="queryStore.results" />
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { ref, watch, onMounted, onUnmounted } from 'vue'
import { useQueryStore } from '@/stores/query'
import TableView from './TableView.vue'
import JsonView from './JsonView.vue'
import FacetPanel from './FacetPanel.vue'

const queryStore = useQueryStore()

// ── Timing toggle & popover ──────────────────────────────────────────

const showTimingDetail = ref(false)
const fastAnimKey = ref(0)
const isFast = ref(false)

watch(() => queryStore.isExecuting, (running, was) => {
  if (was && !running) {
    showTimingDetail.value = false
    if (queryStore.timeMs > 0 && queryStore.timeMs < 100) {
      isFast.value = true
      fastAnimKey.value++
    } else {
      isFast.value = false
    }
  }
})

function onDocClick() {
  showTimingDetail.value = false
}

onMounted(() => document.addEventListener('click', onDocClick))
onUnmounted(() => document.removeEventListener('click', onDocClick))

// ── Formatting ───────────────────────────────────────────────────────

function formatTimeUs(us: number): string {
  if (us >= 1_000_000) return `${(us / 1_000_000).toFixed(2)}s`
  if (us >= 1_000) return `${(us / 1_000).toFixed(2)}ms`
  return `${us}\u00B5s`
}

function formatServerTime(r: any): string {
  const us = r.search_time_us
  if (us != null) return formatTimeUs(us)
  const ms = r.search_time_ms
  if (ms != null && ms > 0) return `${ms}ms`
  return '< 1ms'
}

function formatTimeMs(ms: number | undefined): string {
  if (ms === undefined || ms === null) return '0\u00B5s'
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
  if (ms >= 1) return `${ms.toFixed(2)}ms`
  return `${Math.round(ms * 1000)}\u00B5s`
}
</script>

<style scoped>
.result-panel {
  height: 100%;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.result-panel__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  border-bottom: 1px solid var(--tq-border);
  padding-right: 16px;
  flex-shrink: 0;
}

.result-panel__stats {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: var(--tq-font-size-xs);
  color: var(--tq-text-muted);
}

.result-panel__stats strong {
  color: var(--tv-text-secondary);
  font-weight: 600;
}

.result-panel__sep {
  width: 1px;
  height: 12px;
  background: var(--tv-border-default);
}

/* ── Timing toggle ─────────────────────────────────────────────────── */

.result-panel__timing-wrap {
  position: relative;
}

.result-panel__timing-toggle {
  position: relative;
  padding: 2px 8px;
  border-radius: var(--tv-radius-md);
  cursor: pointer;
  font-variant-numeric: tabular-nums;
  transition: color 0.15s;
}

.result-panel__timing-toggle:hover {
  color: var(--tv-text-primary);
}

.result-panel__timing-toggle--fast {
  color: var(--tv-status-success);
}

.result-panel__timing-toggle--fast::before {
  content: '';
  position: absolute;
  inset: -1px;
  border-radius: var(--tv-radius-md);
  background: conic-gradient(
    from var(--_border-angle),
    transparent 0%,
    transparent 65%,
    color-mix(in srgb, var(--tv-status-success) 40%, transparent) 74%,
    var(--tv-status-success) 80%,
    color-mix(in srgb, var(--tv-status-success) 40%, transparent) 86%,
    transparent 95%,
    transparent 100%
  );
  z-index: -1;
  animation: border-spin 2.8s linear forwards;
}

.result-panel__timing-toggle--fast::after {
  content: '';
  position: absolute;
  inset: 1px;
  border-radius: calc(var(--tv-radius-md) - 1px);
  background: var(--tv-bg-surface);
  z-index: -1;
}

@property --_border-angle {
  syntax: '<angle>';
  initial-value: 0deg;
  inherits: false;
}

@keyframes border-spin {
  0%   { --_border-angle: 0deg;    opacity: 0; }
  3%   {                           opacity: 1; }
  75%  { --_border-angle: 720deg;  opacity: 1; }
  100% { --_border-angle: 720deg;  opacity: 0; }
}

/* ── Timing popover ────────────────────────────────────────────────── */

.result-panel__timing-popover {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  z-index: 200;
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  padding: 8px 12px;
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-muted);
  box-shadow: var(--tv-shadow-md);
  min-width: 160px;
}

.result-panel__timing-row {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  padding: 2px 0;
}

.result-panel__timing-value {
  font-variant-numeric: tabular-nums;
  color: var(--tv-text-primary);
}

.result-panel__timing-label {
  color: var(--tv-text-muted);
}

.result-panel__timing-divider {
  border-top: 1px solid var(--tv-border-default);
  margin: 4px 0;
}

/* ── Rest ──────────────────────────────────────────────────────────── */

.result-panel__error {
  padding: 16px;
  color: var(--tq-error);
  font-size: var(--tq-font-size-sm);
  background: var(--tq-error-dim);
  margin: 8px;
  border-radius: var(--tq-radius-sm);
}

.result-panel__body {
  flex: 1;
  display: flex;
  overflow: hidden;
}

.result-panel__content {
  flex: 1;
  overflow: auto;
}
</style>
