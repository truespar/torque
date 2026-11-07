<script setup lang="ts">
import { ref, computed, watch } from 'vue'
import { useConnectionStore } from '../../stores/connection'
import * as api from '../../utils/api'

const connection = useConnectionStore()

const query = ref('')
const results = ref<any>(null)
const error = ref('')
const loading = ref(false)
const httpTimeMs = ref(0)
const expandedHit = ref<number | null>(null)

// Search settings
const perPage = ref(10)
const numTypos = ref(2)
const prefixEnabled = ref(true)
const showSettings = ref(false)

let debounceTimer: ReturnType<typeof setTimeout> | null = null

const searchableFields = computed(() => {
  const col = connection.collectionList.find((c: any) => c.name === connection.currentCollection)
  if (!col) return []
  return col.fields
    .filter((f: any) => (f.type === 'string' || f.type === 'string[]') && f.index !== false)
    .map((f: any) => f.name)
})

const queryByStr = computed(() => searchableFields.value.join(','))

const primaryField = computed(() => searchableFields.value[0] || '')

const collectionDocCount = computed(() => {
  const col = connection.collectionList.find((c: any) => c.name === connection.currentCollection)
  return col?.num_documents ?? 0
})

async function doSearch() {
  const q = query.value.trim()
  if (!q || !connection.currentCollection || !queryByStr.value) {
    results.value = null
    return
  }

  loading.value = true
  error.value = ''
  const start = performance.now()

  try {
    const res = await api.search(connection.currentCollection, {
      q,
      query_by: queryByStr.value,
      highlight_fields: queryByStr.value,
      highlight_start_tag: '<mark>',
      highlight_end_tag: '</mark>',
      per_page: perPage.value,
      num_typos: numTypos.value,
      prefix: prefixEnabled.value,
    })
    httpTimeMs.value = Math.round((performance.now() - start) * 10) / 10
    results.value = res
  } catch (e: any) {
    error.value = e.message || 'Search failed'
    results.value = null
  } finally {
    loading.value = false
  }
}

function onInput() {
  if (debounceTimer) clearTimeout(debounceTimer)
  debounceTimer = setTimeout(doSearch, 300)
}

function toggleExpand(idx: number) {
  expandedHit.value = expandedHit.value === idx ? null : idx
}

function getHighlightedValue(hit: any, fieldName: string): string | null {
  if (!hit.highlights) return null
  const hl = hit.highlights.find((h: any) => h.field === fieldName)
  if (!hl) return null
  // string[] fields use values array; string fields use value
  if (hl.values) return hl.values.join(', ')
  return hl.value || hl.snippet || null
}

function getRawValue(hit: any, fieldName: string): string {
  const val = hit.document?.[fieldName]
  if (val === undefined || val === null) return ''
  if (Array.isArray(val)) return val.join(', ')
  return String(val)
}

// Clear state on collection change
watch(() => connection.currentCollection, () => {
  query.value = ''
  results.value = null
  error.value = ''
  expandedHit.value = null
})
</script>

<template>
  <div class="tq-panel qs">
    <h2>Quick Search</h2>

    <!-- No collection selected -->
    <div v-if="!connection.currentCollection" class="tq-empty">
      Select a collection to search
    </div>

    <template v-else>
      <!-- Search input -->
      <div class="qs__input-row">
        <div class="qs__input-wrap">
          <svg class="qs__search-icon" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="11" cy="11" r="8" />
            <path d="m21 21-4.3-4.3" />
          </svg>
          <input
            v-model="query"
            class="qs__input"
            type="text"
            :placeholder="`Search ${connection.currentCollection}...`"
            @input="onInput"
          />
          <span v-if="loading" class="qs__spinner" />
        </div>

        <!-- Timing badge -->
        <span v-if="results && !loading" class="qs__timing">
          {{ results.search_time_ms ?? (results.search_time_us ? (results.search_time_us / 1000).toFixed(1) : httpTimeMs) }}ms
          &middot;
          {{ (results.found ?? 0).toLocaleString() }} found
        </span>

        <button class="qs__settings-btn" title="Search settings" @click="showSettings = !showSettings">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
            <circle cx="12" cy="12" r="3" />
          </svg>
        </button>
      </div>

      <!-- Settings row -->
      <div v-if="showSettings" class="qs__settings">
        <label class="qs__setting">
          <span>Per page</span>
          <select class="fg-select" v-model.number="perPage" @change="query.trim() && doSearch()">
            <option :value="5">5</option>
            <option :value="10">10</option>
            <option :value="25">25</option>
            <option :value="50">50</option>
          </select>
        </label>
        <label class="qs__setting">
          <span>Typo tolerance</span>
          <select class="fg-select" v-model.number="numTypos" @change="query.trim() && doSearch()">
            <option :value="0">Off</option>
            <option :value="1">1</option>
            <option :value="2">2</option>
          </select>
        </label>
        <label class="qs__setting" @click.prevent="prefixEnabled = !prefixEnabled; query.trim() && doSearch()">
          <span class="fg-checkbox">
            <input type="checkbox" :checked="prefixEnabled" />
            <span class="fg-checkbox__box" />
          </span>
          <span>Prefix</span>
        </label>
      </div>

      <!-- Empty query state -->
      <div v-if="!query.trim() && !results" class="qs__empty-state">
        <div class="qs__empty-stats">
          {{ collectionDocCount.toLocaleString() }} documents &middot;
          {{ searchableFields.length }} searchable fields
        </div>
        <div class="qs__empty-hint">Start typing to search...</div>
      </div>

      <!-- Error -->
      <div v-if="error" class="qs__error">{{ error }}</div>

      <!-- No results -->
      <div v-if="results && results.found === 0" class="qs__no-results">
        No results for "{{ query }}"
      </div>

      <!-- Results -->
      <div v-if="results && results.found > 0" class="qs__results">
        <div
          v-for="(hit, idx) in results.hits"
          :key="idx"
          class="qs__hit"
          @click="toggleExpand(Number(idx))"
        >
          <!-- Primary field -->
          <div class="qs__hit-primary">
            <span
              v-if="primaryField && getHighlightedValue(hit, primaryField)"
              v-html="getHighlightedValue(hit, primaryField)"
            />
            <span v-else-if="primaryField">{{ getRawValue(hit, primaryField) }}</span>
            <span v-else class="qs__hit-id">{{ hit.document?.id }}</span>
          </div>

          <!-- Secondary fields -->
          <div class="qs__hit-fields">
            <template v-for="field in searchableFields.slice(1)" :key="field">
              <div v-if="getRawValue(hit, field)" class="qs__hit-field">
                <span class="qs__hit-field-name">{{ field }}</span>
                <span
                  v-if="getHighlightedValue(hit, field)"
                  class="qs__hit-field-value"
                  v-html="getHighlightedValue(hit, field)"
                />
                <span v-else class="qs__hit-field-value">{{ getRawValue(hit, field) }}</span>
              </div>
            </template>
          </div>

          <!-- Score badge -->
          <div class="qs__hit-meta">
            <span class="qs__score-badge">{{ hit.text_match_info?.score ?? hit.text_match ?? '' }}</span>
          </div>

          <!-- Expanded: full document JSON -->
          <div v-if="expandedHit === idx" class="qs__hit-json" @click.stop>
            <pre>{{ JSON.stringify(hit.document, null, 2) }}</pre>
          </div>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped>
.qs {
  overflow-y: auto;
}

.qs__input-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
}

.qs__input-wrap {
  flex: 1;
  position: relative;
  display: flex;
  align-items: center;
}

.qs__search-icon {
  position: absolute;
  left: 10px;
  color: var(--tv-text-muted);
  pointer-events: none;
}

.qs__input {
  width: 100%;
  padding: 10px 12px 10px 34px;
  font-size: 15px;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md, 6px);
  background: var(--tv-bg-input, var(--tv-bg-surface));
  color: var(--tv-text-primary);
  outline: none;
  transition: border-color 0.15s;
}
.qs__input:focus {
  border-color: var(--tv-accent);
}
.qs__input::placeholder {
  color: var(--tv-text-muted);
}

.qs__spinner {
  position: absolute;
  right: 10px;
  width: 14px;
  height: 14px;
  border: 2px solid var(--tv-border-default);
  border-top-color: var(--tv-accent);
  border-radius: 50%;
  animation: qs-spin 0.6s linear infinite;
}
@keyframes qs-spin {
  to { transform: rotate(360deg); }
}

.qs__timing {
  font-size: 12px;
  color: var(--tv-text-secondary);
  white-space: nowrap;
  flex-shrink: 0;
}

.qs__settings-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: none;
  border-radius: var(--tv-radius-sm, 4px);
  background: transparent;
  color: var(--tv-text-muted);
  cursor: pointer;
  flex-shrink: 0;
}
.qs__settings-btn:hover {
  color: var(--tv-text-primary);
  background: var(--tv-bg-hover);
}

.qs__settings {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 6px 0 10px;
  font-size: 12px;
  color: var(--tv-text-secondary);
}
.qs__setting {
  display: flex;
  align-items: center;
  gap: 4px;
}
.qs__setting .fg-select {
  height: 26px;
  font-size: 12px;
}
.qs__setting .fg-checkbox {
  flex-shrink: 0;
}

.qs__empty-state {
  text-align: center;
  padding: 48px 16px;
  color: var(--tv-text-muted);
}
.qs__empty-stats {
  font-size: 14px;
  margin-bottom: 8px;
}
.qs__empty-hint {
  font-size: 13px;
  opacity: 0.7;
}

.qs__error {
  padding: 8px 12px;
  border-radius: var(--tv-radius-sm, 4px);
  background: rgba(220, 38, 38, 0.1);
  color: #ef4444;
  font-size: 13px;
}

.qs__no-results {
  text-align: center;
  padding: 32px 16px;
  color: var(--tv-text-muted);
  font-size: 14px;
}

.qs__results {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.qs__hit {
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md, 6px);
  padding: 12px 14px;
  cursor: pointer;
  transition: border-color 0.15s, background 0.15s;
  position: relative;
}
.qs__hit:hover {
  border-color: var(--tv-accent);
  background: var(--tv-bg-hover);
}

.qs__hit-primary {
  font-size: 14px;
  font-weight: 500;
  color: var(--tv-text-primary);
  margin-bottom: 4px;
  line-height: 1.4;
}
.qs__hit-primary :deep(mark) {
  background: #facc15;
  color: #000;
  border-radius: 2px;
  padding: 0 1px;
}

.qs__hit-fields {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.qs__hit-field {
  display: flex;
  gap: 6px;
  font-size: 12px;
  line-height: 1.4;
}
.qs__hit-field-name {
  color: var(--tv-text-muted);
  flex-shrink: 0;
  min-width: 80px;
}
.qs__hit-field-value {
  color: var(--tv-text-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.qs__hit-field-value :deep(mark) {
  background: #facc15;
  color: #000;
  border-radius: 2px;
  padding: 0 1px;
}

.qs__hit-meta {
  position: absolute;
  top: 10px;
  right: 12px;
}
.qs__score-badge {
  font-size: 10px;
  font-family: var(--tv-font-mono, monospace);
  color: var(--tv-text-muted);
  background: var(--tv-bg-elevated, var(--tv-bg-surface));
  padding: 1px 5px;
  border-radius: var(--tv-radius-sm, 4px);
}

.qs__hit-json {
  margin-top: 8px;
  padding: 8px;
  border-top: 1px solid var(--tv-border-default);
  overflow-x: auto;
}
.qs__hit-json pre {
  margin: 0;
  font-size: 11px;
  font-family: var(--tv-font-mono, monospace);
  color: var(--tv-text-secondary);
  white-space: pre-wrap;
  word-break: break-all;
}

.qs__hit-id {
  color: var(--tv-text-muted);
  font-family: var(--tv-font-mono, monospace);
}
</style>
