<template>
  <div class="ep-panel">
    <!-- Sample Datasets -->
    <section class="ep-section">
      <h2 class="ep-heading">Sample Datasets</h2>
      <p class="ep-subtext">Load a sample collection to explore search, filtering, faceting, and sorting.</p>

      <div class="ep-cards">
        <div v-for="ds in SAMPLE_DATASETS" :key="ds.id" class="ep-card">
          <div class="ep-card-header">
            <svg class="ep-card-icon" viewBox="0 0 24 24" fill="currentColor"><path :d="ds.icon" /></svg>
            <div class="ep-card-info">
              <div class="ep-card-name">{{ ds.name }}</div>
              <div class="ep-card-desc">{{ ds.description }}</div>
            </div>
          </div>
          <div class="ep-card-meta">
            <span class="ep-badge">{{ ds.schema.fields.length }} fields</span>
            <span class="ep-badge">{{ ds.documents.split('\n').length }} docs</span>
          </div>
          <button class="fg-btn fg-btn--primary fg-btn--sm ep-card-btn" @click="confirmLoad(ds)">
            Load Data
          </button>
        </div>
      </div>
    </section>

    <!-- Query Examples -->
    <section class="ep-section">
      <h2 class="ep-heading">Query Examples</h2>
      <p class="ep-subtext">Click an example to load it into the editor.</p>

      <div v-for="cat in categories" :key="cat.name" class="ep-category">
        <button class="ep-category-header" @click="toggleCategory(cat.name)">
          <svg class="ep-chevron" :class="{ 'ep-chevron--open': openCategories.has(cat.name) }" viewBox="0 0 24 24" fill="currentColor">
            <path d="M10 6L8.59 7.41 13.17 12l-4.58 4.59L10 18l6-6z" />
          </svg>
          <span>{{ cat.name }}</span>
          <span class="ep-category-count">{{ cat.examples.length }}</span>
        </button>
        <div v-if="openCategories.has(cat.name)" class="ep-category-body">
          <div
            v-for="ex in cat.examples"
            :key="ex.name"
            class="ep-example"
            @click="loadExample(ex.query)"
          >
            <div class="ep-example-name">{{ ex.name }}</div>
            <pre class="ep-example-code">{{ ex.query }}</pre>
          </div>
        </div>
      </div>
    </section>

    <!-- Load Confirmation Modal -->
    <Teleport to="body">
      <Transition name="modal">
        <div v-if="pendingDataset" class="fg-modal-overlay" @mousedown.self="pendingDataset = null">
          <div class="ep-modal" role="dialog" aria-modal="true" aria-label="Load sample dataset">
            <div class="ep-modal-header">
              <span>Load Sample Dataset</span>
              <button class="fg-btn fg-btn--ghost fg-btn--icon" aria-label="Close" @click="pendingDataset = null">
                <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><path d="M19 6.41L17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z" /></svg>
              </button>
            </div>
            <div class="ep-modal-body">
              <p>
                This will create a collection called
                <strong>{{ pendingDataset.schema.name }}</strong>
                with <strong>{{ pendingDataset.documents.split('\n').length }}</strong> documents.
              </p>
              <p v-if="collectionExists" class="ep-modal-warning">
                Collection "{{ pendingDataset.schema.name }}" already exists. Documents will be upserted.
              </p>
              <div class="ep-modal-fields">
                <div class="ep-modal-field" v-for="f in pendingDataset.schema.fields" :key="f.name">
                  <span class="ep-modal-field-name">{{ f.name }}</span>
                  <span class="ep-modal-field-type">{{ f.type }}</span>
                  <span v-if="f.facet" class="ep-badge ep-badge--sm">facet</span>
                  <span v-if="f.sort" class="ep-badge ep-badge--sm">sort</span>
                  <span v-if="f.index" class="ep-badge ep-badge--sm">index</span>
                </div>
              </div>
            </div>
            <div class="ep-modal-footer">
              <button class="fg-btn fg-btn--ghost" @click="pendingDataset = null">Cancel</button>
              <button class="fg-btn fg-btn--primary" :disabled="loading" @click="doLoad">
                {{ loading ? 'Loading...' : collectionExists ? 'Upsert Data' : 'Create & Load' }}
              </button>
            </div>
          </div>
        </div>
      </Transition>
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, reactive } from 'vue'
import { useQueryStore } from '@/stores/query'
import { useSettingsStore } from '@/stores/settings'
import { useConnectionStore } from '@/stores/connection'
import { useToast } from '@/composables/useToast'
import * as api from '@/utils/api'
import { SAMPLE_DATASETS, type SampleDataset } from '@/data/samples'

const queryStore = useQueryStore()
const settings = useSettingsStore()
const connection = useConnectionStore()
const toast = useToast()

// ── Sample Datasets ──────────────────────────────────────────────────

const pendingDataset = ref<SampleDataset | null>(null)
const loading = ref(false)

const collectionExists = computed(() => {
  if (!pendingDataset.value) return false
  return connection.collectionNames.includes(pendingDataset.value.schema.name)
})

function confirmLoad(ds: SampleDataset) {
  pendingDataset.value = ds
}

async function doLoad() {
  const ds = pendingDataset.value
  if (!ds) return
  loading.value = true
  try {
    const exists = connection.collectionNames.includes(ds.schema.name)
    if (!exists) {
      await api.createCollection(ds.schema)
    }
    await api.importDocuments(ds.schema.name, ds.documents, exists ? 'upsert' : 'create')
    await connection.fetchCollections()
    connection.setCurrentCollection(ds.schema.name)
    const queryBy = ds.schema.fields
      .filter(f => (f.type === 'string' || f.type === 'string[]') && f.index !== false && !f.name.includes('.'))
      .map(f => f.name)
      .join(',')
    queryStore.currentQuery = JSON.stringify({ q: '*', query_by: queryBy, per_page: 10 }, null, 2)
    toast.success(`Loaded ${ds.name} — ${ds.documents.split('\n').length} documents imported`)
    pendingDataset.value = null
    settings.activePanel = null
  } catch (e: any) {
    toast.error(e.message || 'Failed to load sample dataset')
  } finally {
    loading.value = false
  }
}

// ── Query Examples ───────────────────────────────────────────────────

const openCategories = reactive(new Set<string>())

function toggleCategory(name: string) {
  if (openCategories.has(name)) {
    openCategories.delete(name)
  } else {
    openCategories.add(name)
  }
}

function loadExample(query: string) {
  queryStore.currentQuery = query
  settings.activePanel = null
}

const categories = [
  {
    name: 'Basic Text Search',
    examples: [
      { name: 'Simple search', query: '{\n  "q": "running shoes",\n  "query_by": "title,description",\n  "per_page": 10\n}' },
      { name: 'Wildcard (all docs)', query: '{\n  "q": "*",\n  "per_page": 20\n}' },
      { name: 'Prefix search', query: '{\n  "q": "run",\n  "query_by": "title",\n  "prefix": true,\n  "per_page": 10\n}' },
    ],
  },
  {
    name: 'Filtered Search',
    examples: [
      { name: 'Numeric filter', query: '{\n  "q": "*",\n  "filter_by": "price:>50 && price:<200",\n  "per_page": 10\n}' },
      { name: 'String filter', query: '{\n  "q": "*",\n  "filter_by": "brand:=Nike",\n  "per_page": 10\n}' },
      { name: 'Combined filters', query: '{\n  "q": "shoes",\n  "query_by": "title",\n  "filter_by": "price:>50 && brand:=[Nike, Adidas]",\n  "per_page": 10\n}' },
    ],
  },
  {
    name: 'Faceted Search',
    examples: [
      { name: 'Facets', query: '{\n  "q": "*",\n  "facet_by": "brand,category",\n  "max_facet_values": 10,\n  "per_page": 5\n}' },
      { name: 'Facets with filter', query: '{\n  "q": "shoes",\n  "query_by": "title",\n  "facet_by": "brand",\n  "filter_by": "price:>100",\n  "per_page": 10\n}' },
    ],
  },
  {
    name: 'Sorted Search',
    examples: [
      { name: 'Sort by field', query: '{\n  "q": "*",\n  "sort_by": "price:asc",\n  "per_page": 10\n}' },
      { name: 'Multi-field sort', query: '{\n  "q": "shoes",\n  "query_by": "title",\n  "sort_by": "_text_match:desc,price:asc",\n  "per_page": 10\n}' },
    ],
  },
  {
    name: 'Geo Search',
    examples: [
      { name: 'Geo radius', query: '{\n  "q": "*",\n  "filter_by": "location:(48.85, 2.35, 10 km)",\n  "sort_by": "location(48.85, 2.35):asc",\n  "per_page": 10\n}' },
    ],
  },
  {
    name: 'Group By',
    examples: [
      { name: 'Group results', query: '{\n  "q": "*",\n  "group_by": "brand",\n  "group_limit": 3,\n  "per_page": 10\n}' },
    ],
  },
  {
    name: 'Nested Fields',
    examples: [
      { name: 'Filter nested object', query: '{\n  "q": "*",\n  "filter_by": "metadata.brand:=NordWear",\n  "per_page": 10\n}' },
      { name: 'Facet nested field', query: '{\n  "q": "*",\n  "facet_by": "metadata.brand,variants.color",\n  "per_page": 10\n}' },
      { name: 'Element-level AND', query: '{\n  "q": "*",\n  "filter_by": "variants:(color:=Black && size:=M)",\n  "per_page": 10\n}' },
    ],
  },
  {
    name: 'Vector Search',
    examples: [
      { name: 'Basic vector search', query: '{\n  "q": "*",\n  "vector_query": "embedding:([0.1, 0.2, 0.3, ...], k:10)",\n  "per_page": 10\n}' },
      { name: 'Find similar by ID', query: '{\n  "q": "*",\n  "vector_query": "embedding:(id:doc_123, k:10)",\n  "per_page": 10\n}' },
      { name: 'Distance threshold', query: '{\n  "q": "*",\n  "vector_query": "embedding:([0.1, 0.2, 0.3, ...], k:20, distance_threshold:0.5)",\n  "per_page": 10\n}' },
      { name: 'Hybrid search (RRF)', query: '{\n  "q": "machine learning",\n  "query_by": "title,body",\n  "vector_query": "embedding:([0.1, 0.2, 0.3, ...], k:20)",\n  "per_page": 10\n}' },
      { name: 'Hybrid with alpha', query: '{\n  "q": "machine learning",\n  "query_by": "title,body",\n  "vector_query": "embedding:([0.1, 0.2, 0.3, ...], k:20, alpha:0.7)",\n  "per_page": 10\n}' },
      { name: 'Sort by vector distance', query: '{\n  "q": "*",\n  "vector_query": "embedding:([0.1, 0.2, 0.3, ...], k:20)",\n  "sort_by": "_vector_distance:asc",\n  "per_page": 10\n}' },
      { name: 'Tuned HNSW recall', query: '{\n  "q": "*",\n  "vector_query": "embedding:([0.1, 0.2, 0.3, ...], k:10, ef:200)",\n  "per_page": 10\n}' },
      { name: 'Filtered vector search', query: '{\n  "q": "*",\n  "filter_by": "category:=AI",\n  "vector_query": "embedding:([0.1, 0.2, 0.3, ...], k:10, flat_search_cutoff:500)",\n  "per_page": 10\n}' },
    ],
  },
]
</script>

<style scoped>
.ep-panel {
  padding: 20px;
  overflow-y: auto;
  height: 100%;
}

.ep-section {
  margin-bottom: 32px;
}

.ep-heading {
  font-size: var(--tv-font-size-lg);
  font-weight: 600;
  color: var(--tv-text-primary);
  margin: 0 0 4px;
}

.ep-subtext {
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-secondary);
  margin: 0 0 16px;
}

/* ── Dataset Cards ─────────────────────────────────────────────────── */

.ep-cards {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.ep-card {
  background: var(--tv-bg-surface);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-lg);
  padding: 14px 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
  transition: border-color 0.15s;
}

.ep-card:hover {
  border-color: var(--tv-accent);
}

.ep-card-header {
  display: flex;
  gap: 12px;
  align-items: flex-start;
}

.ep-card-icon {
  width: 28px;
  height: 28px;
  flex-shrink: 0;
  color: var(--tv-accent);
  margin-top: 1px;
}

.ep-card-info {
  min-width: 0;
}

.ep-card-name {
  font-weight: 600;
  font-size: var(--tv-font-size-sm);
  color: var(--tv-text-primary);
}

.ep-card-desc {
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-secondary);
  margin-top: 2px;
  line-height: 1.4;
}

.ep-card-meta {
  display: flex;
  gap: 6px;
}

.ep-badge {
  display: inline-flex;
  align-items: center;
  font-size: 11px;
  font-weight: 500;
  padding: 2px 7px;
  border-radius: var(--tv-radius-sm);
  background: var(--tv-accent-subtle);
  color: var(--tv-accent);
}

.ep-badge--sm {
  font-size: 10px;
  padding: 1px 5px;
}

.ep-card-btn {
  align-self: flex-start;
}

/* ── Query Examples ────────────────────────────────────────────────── */

.ep-category {
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  margin-bottom: 6px;
  overflow: hidden;
}

.ep-category-header {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 8px 12px;
  background: var(--tv-bg-surface);
  border: none;
  color: var(--tv-text-primary);
  font-size: var(--tv-font-size-sm);
  font-weight: 600;
  cursor: pointer;
  text-align: left;
  font-family: inherit;
}

.ep-category-header:hover {
  background: var(--tv-bg-hover);
}

.ep-chevron {
  width: 18px;
  height: 18px;
  flex-shrink: 0;
  color: var(--tv-text-secondary);
  transition: transform 0.15s;
}

.ep-chevron--open {
  transform: rotate(90deg);
}

.ep-category-count {
  margin-left: auto;
  font-size: 11px;
  font-weight: 400;
  color: var(--tv-text-muted);
}

.ep-category-body {
  border-top: 1px solid var(--tv-border-subtle);
}

.ep-example {
  padding: 10px 14px;
  cursor: pointer;
  transition: background 0.1s;
}

.ep-example:hover {
  background: var(--tv-bg-hover);
}

.ep-example + .ep-example {
  border-top: 1px solid var(--tv-border-subtle);
}

.ep-example-name {
  font-weight: 500;
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-primary);
  margin-bottom: 6px;
}

.ep-example-code {
  font-family: var(--tv-font-mono);
  font-size: 12px;
  color: var(--tv-text-secondary);
  background: var(--tv-bg-inset);
  padding: 8px 10px;
  border-radius: var(--tv-radius-sm);
  overflow-x: auto;
  margin: 0;
  line-height: 1.5;
}

/* ── Modal ─────────────────────────────────────────────────────────── */

.ep-modal {
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-xl);
  box-shadow: var(--tv-shadow-lg);
  width: 440px;
  max-height: 80vh;
  display: flex;
  flex-direction: column;
}

.ep-modal-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 20px;
  border-bottom: 1px solid var(--tv-border-subtle);
  font-weight: 600;
  font-size: var(--tv-font-size-sm);
  color: var(--tv-text-primary);
}

.ep-modal-body {
  padding: 16px 20px;
  overflow-y: auto;
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-secondary);
  line-height: 1.5;
}

.ep-modal-body p {
  margin: 0 0 12px;
}

.ep-modal-body strong {
  color: var(--tv-text-primary);
}

.ep-modal-warning {
  padding: 8px 12px;
  border-radius: var(--tv-radius-md);
  background: var(--tv-status-warning-subtle);
  color: var(--tv-status-warning);
  font-size: var(--tv-font-size-xs);
}

.ep-modal-fields {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.ep-modal-field {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 0;
  font-size: var(--tv-font-size-xs);
}

.ep-modal-field-name {
  font-family: var(--tv-font-mono);
  color: var(--tv-text-primary);
  font-weight: 500;
}

.ep-modal-field-type {
  font-family: var(--tv-font-mono);
  color: var(--tv-text-muted);
  font-size: 11px;
}

.ep-modal-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 20px;
  border-top: 1px solid var(--tv-border-subtle);
}

/* ── Modal transition ──────────────────────────────────────────────── */

.modal-enter-active,
.modal-leave-active {
  transition: opacity 0.15s;
}

.modal-enter-active .ep-modal,
.modal-leave-active .ep-modal {
  transition: transform 0.15s;
}

.modal-enter-from,
.modal-leave-to {
  opacity: 0;
}

.modal-enter-from .ep-modal,
.modal-leave-to .ep-modal {
  transform: scale(0.96);
}
</style>
