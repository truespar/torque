<template>
  <div class="table-view" :class="{ 'table-view--split': expanded >= 0 }">
    <div class="table-view__table-wrap">
      <table v-if="hits.length">
        <thead>
          <tr>
            <th v-for="col in columns" :key="col">{{ col }}</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="(hit, i) in hits"
            :key="i"
            class="table-view__row"
            :class="{ 'table-view__row--active': expanded === i }"
            @click="expanded = expanded === i ? -1 : i"
          >
            <td v-for="col in columns" :key="col">
              <span v-if="getHighlight(hit, col)" v-html="getHighlight(hit, col)" />
              <span v-else>{{ formatCell(hit.document?.[col]) }}</span>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-else class="tq-empty">No results</div>
    </div>

    <div v-if="expanded >= 0" class="table-view__detail">
      <div class="table-view__detail-header">
        <span class="table-view__detail-title">Document</span>
        <div class="table-view__detail-tabs">
          <button
            class="table-view__detail-tab"
            :class="{ active: detailTab === 'highlights' && hasHighlights(expanded) }"
            :disabled="!hasHighlights(expanded)"
            @click="detailTab = 'highlights'"
          >Highlights</button>
          <button
            class="table-view__detail-tab"
            :class="{ active: detailTab === 'json' || !hasHighlights(expanded) }"
            @click="detailTab = 'json'"
          >JSON</button>
        </div>
        <button class="table-view__copy-btn" @click="copyJson(expanded)" :title="copied === expanded ? 'Copied!' : 'Copy JSON'">
          <svg v-if="copied !== expanded" width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d="M16 1H4c-1.1 0-2 .9-2 2v14h2V3h12V1zm3 4H8c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z"/></svg>
          <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d="M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/></svg>
          <span>{{ copied === expanded ? 'Copied' : 'Copy' }}</span>
        </button>
        <button class="table-view__close-btn" @click="expanded = -1" title="Close">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"/></svg>
        </button>
      </div>

      <!-- Highlights tab -->
      <div v-if="detailTab === 'highlights' && hasHighlights(expanded)" class="table-view__highlights">
        <!-- text_match_info bar -->
        <div v-if="hits[expanded]?.text_match_info" class="table-view__match-info">
          <span class="table-view__match-badge">{{ hits[expanded].text_match_info.tokens_matched }} token{{ hits[expanded].text_match_info.tokens_matched === 1 ? '' : 's' }} matched</span>
          <span class="table-view__match-badge">{{ hits[expanded].text_match_info.fields_matched }} field{{ hits[expanded].text_match_info.fields_matched === 1 ? '' : 's' }}</span>
          <span v-if="hits[expanded].text_match_info.num_tokens_dropped > 0" class="table-view__match-badge table-view__match-badge--warn">{{ hits[expanded].text_match_info.num_tokens_dropped }} dropped</span>
        </div>
        <!-- Per-field highlights -->
        <div v-for="hl in getHighlightList(expanded)" :key="hl.field" class="table-view__hl-field">
          <div class="table-view__hl-label">{{ hl.field }}</div>
          <div v-if="hl.value" class="table-view__hl-text" v-html="hl.value" />
          <div v-else-if="hl.snippet" class="table-view__hl-text" v-html="hl.snippet" />
          <template v-if="hl.values?.length">
            <div v-for="(v, vi) in hl.values" :key="vi" class="table-view__hl-text table-view__hl-array-item">
              <span class="table-view__hl-index">[{{ hl.indices?.[vi] ?? vi }}]</span>
              <span v-html="v" />
            </div>
          </template>
          <div class="table-view__hl-tokens">
            <span v-for="(t, ti) in flatMatchedTokens(hl.matched_tokens)" :key="ti" class="table-view__token-pill">{{ t }}</span>
          </div>
        </div>
      </div>

      <!-- JSON tab -->
      <pre v-else class="table-view__json">{{ JSON.stringify(hits[expanded]?.document, null, 2) }}</pre>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue'

const props = defineProps<{ hits: any[] }>()
const expanded = ref(-1)
const copied = ref(-1)
const detailTab = ref<'highlights' | 'json'>('highlights')

// Auto-switch to highlights tab when expanding a hit with highlights
watch(expanded, (i) => {
  if (i >= 0 && hasHighlights(i)) {
    detailTab.value = 'highlights'
  } else {
    detailTab.value = 'json'
  }
})

function copyJson(i: number) {
  const json = JSON.stringify(props.hits[i]?.document, null, 2)
  navigator.clipboard.writeText(json)
  copied.value = i
  setTimeout(() => { copied.value = -1 }, 1500)
}

const columns = computed(() => {
  if (!props.hits.length) return []
  const doc = props.hits[0]?.document
  if (!doc) return []
  return Object.keys(doc).filter(k => k !== 'id').slice(0, 20)
})

function formatCell(val: unknown): string {
  if (val === null || val === undefined) return ''
  if (typeof val === 'object') return JSON.stringify(val)
  return String(val)
}

function getHighlight(hit: any, field: string): string | null {
  // Check highlights array
  const hl = hit.highlights?.find((h: any) => h.field === field)
  if (hl?.snippet) return hl.snippet
  if (hl?.value) return hl.value
  if (hl?.snippets) return hl.snippets.join(', ')
  if (hl?.values) return hl.values.join(', ')
  // Check highlight object (new-style)
  const hlObj = hit.highlight?.[field]
  if (hlObj?.snippet) return hlObj.snippet
  if (hlObj?.value) return hlObj.value
  return null
}

function hasHighlights(i: number): boolean {
  const hit = props.hits[i]
  return hit?.highlights?.length > 0 || (hit?.highlight && Object.keys(hit.highlight).length > 0)
}

function getHighlightList(i: number): any[] {
  const hit = props.hits[i]
  if (hit?.highlights?.length) return hit.highlights
  // Fall back to highlight object
  if (hit?.highlight) {
    return Object.entries(hit.highlight).map(([field, data]: [string, any]) => ({
      field,
      ...data,
    }))
  }
  return []
}

function flatMatchedTokens(tokens: any): string[] {
  if (!tokens) return []
  if (Array.isArray(tokens)) {
    // Could be ["foo", "bar"] or [["foo"], ["bar"]] for arrays
    return tokens.flatMap((t: any) => (Array.isArray(t) ? t : [t]))
  }
  return []
}
</script>

<style scoped>
.table-view {
  height: 100%;
  overflow: auto;
}

.table-view--split {
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.table-view--split .table-view__table-wrap {
  flex: 1;
  min-height: 0;
  overflow: auto;
}

table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--tq-font-size-sm);
}

th {
  text-align: left;
  padding: 8px 12px;
  background: var(--tq-bg-elevated);
  border-bottom: 1px solid var(--tq-border);
  color: var(--tq-text-secondary);
  font-weight: 600;
  font-size: var(--tq-font-size-xs);
  text-transform: uppercase;
  letter-spacing: 0.5px;
  position: sticky;
  top: 0;
  z-index: 1;
  white-space: nowrap;
}

td {
  padding: 8px 12px;
  border-bottom: 1px solid var(--tq-border-subtle);
  max-width: 300px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.table-view__row {
  cursor: pointer;
  transition: background var(--tq-transition);
}

.table-view__row:hover {
  background: var(--tq-bg-hover);
}

.table-view__row--active {
  background: var(--tv-accent-subtle);
}

/* -- Detail panel -------------------------------------------------------- */

.table-view__detail {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  border-top: 1px solid var(--tq-border);
  background: var(--tq-bg-elevated);
}

.table-view__detail-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-bottom: 1px solid var(--tv-border-subtle);
  flex-shrink: 0;
}

.table-view__detail-title {
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-secondary);
  margin-right: 8px;
}

.table-view__detail-tabs {
  display: flex;
  gap: 2px;
  margin-right: auto;
}

.table-view__detail-tab {
  padding: 2px 10px;
  font-size: 11px;
  font-family: inherit;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-sm);
  background: transparent;
  color: var(--tv-text-muted);
  cursor: pointer;
  transition: color 0.15s, background 0.15s;
}

.table-view__detail-tab:hover:not(:disabled) {
  color: var(--tv-text-primary);
}

.table-view__detail-tab:disabled {
  opacity: 0.4;
  cursor: default;
}

.table-view__detail-tab.active {
  background: var(--tq-accent-dim);
  color: var(--tq-accent);
  border-color: var(--tq-accent);
}

.table-view__copy-btn,
.table-view__close-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 8px;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-sm);
  background: var(--tv-bg-surface);
  color: var(--tv-text-secondary);
  font-size: 11px;
  font-family: inherit;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s;
}

.table-view__close-btn {
  padding: 3px 5px;
}

.table-view__copy-btn:hover,
.table-view__close-btn:hover {
  color: var(--tv-text-primary);
  border-color: var(--tv-border-strong);
}

.table-view__json {
  font-family: var(--tq-font-mono);
  font-size: var(--tq-font-size-xs);
  color: var(--tq-text-secondary);
  margin: 0;
  padding: 10px 14px;
  overflow: auto;
  flex: 1;
  min-height: 0;
}

/* -- Highlights tab ------------------------------------------------------ */

.table-view__highlights {
  overflow: auto;
  flex: 1;
  min-height: 0;
  padding: 10px 14px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.table-view__match-info {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}

.table-view__match-badge {
  font-size: 10px;
  padding: 2px 8px;
  border-radius: 10px;
  background: var(--tq-accent-dim);
  color: var(--tq-accent);
  font-weight: 500;
}

.table-view__match-badge--warn {
  background: color-mix(in srgb, var(--tq-warning, #f59e0b) 15%, transparent);
  color: var(--tq-warning, #f59e0b);
}

.table-view__hl-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.table-view__hl-label {
  font-size: var(--tq-font-size-xs);
  font-weight: 600;
  color: var(--tq-text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.table-view__hl-text {
  font-size: var(--tq-font-size-sm);
  color: var(--tq-text-primary);
  line-height: 1.6;
  word-break: break-word;
}

.table-view__hl-array-item {
  display: flex;
  gap: 6px;
  align-items: baseline;
}

.table-view__hl-index {
  font-size: 10px;
  font-family: var(--tq-font-mono);
  color: var(--tq-text-muted);
  flex-shrink: 0;
}

.table-view__hl-tokens {
  display: flex;
  gap: 4px;
  flex-wrap: wrap;
}

.table-view__token-pill {
  font-size: 10px;
  font-family: var(--tq-font-mono);
  padding: 1px 6px;
  border-radius: 3px;
  background: var(--tq-bg-hover);
  color: var(--tq-text-muted);
}

:deep(mark) {
  background: var(--tq-accent-dim);
  color: var(--tq-accent);
  border-radius: 2px;
  padding: 0 2px;
}
</style>
