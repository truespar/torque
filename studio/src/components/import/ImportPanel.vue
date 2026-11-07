<script setup lang="ts">
import { ref } from 'vue'
import { useConnectionStore } from '@/stores/connection'
import { useToast } from '@/composables/useToast'
import * as api from '@/utils/api'

const ACTIONS = ['upsert', 'create', 'update'] as const

const connection = useConnectionStore()
const toast = useToast()

const collection = ref(connection.currentCollection)
const action = ref('upsert')
const jsonl = ref('')
const importing = ref(false)
const result = ref('')

// Dropdown state
const openDropdown = ref<'collection' | 'action' | null>(null)
const dropdownPos = ref({ top: 0, left: 0, width: 0 })

function toggleDropdown(which: 'collection' | 'action', e: MouseEvent) {
  if (openDropdown.value === which) {
    openDropdown.value = null
    return
  }
  const trigger = e.currentTarget as HTMLElement
  const rect = trigger.getBoundingClientRect()
  dropdownPos.value = { top: rect.bottom + 4, left: rect.left, width: rect.width }
  openDropdown.value = which
}

function selectCollection(name: string) {
  collection.value = name
  openDropdown.value = null
}

function selectAction(a: string) {
  action.value = a
  openDropdown.value = null
}

function onFile(e: Event) {
  const file = (e.target as HTMLInputElement).files?.[0]
  if (!file) return
  const reader = new FileReader()
  reader.onload = () => { jsonl.value = reader.result as string }
  reader.readAsText(file)
}

async function doImport() {
  importing.value = true
  result.value = ''
  try {
    const resp = await api.importDocuments(collection.value, jsonl.value, action.value)
    result.value = resp
    toast.success('Import complete')
    connection.fetchCollections()
  } catch (e: any) {
    toast.error(e.message || 'Import failed')
    result.value = e.message || 'Error'
  } finally {
    importing.value = false
  }
}
</script>

<template>
  <div class="ip-root">
    <div class="ip-header">
      <span class="ip-title">Import Documents</span>
    </div>

    <div class="ip-form">
      <div class="ip-row ip-row--horizontal">
        <div class="ip-field">
          <label class="ip-label">Collection</label>
          <div class="ip-dropdown" @click.stop>
            <button class="ip-dropdown-trigger" @click="toggleDropdown('collection', $event)">
              <span :class="{ 'ip-placeholder': !collection }">{{ collection || 'Select collection...' }}</span>
              <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5">
                <path d="m6 9 6 6 6-6" />
              </svg>
            </button>
          </div>
        </div>
        <div class="ip-field ip-field--action">
          <label class="ip-label">Action</label>
          <div class="ip-dropdown" @click.stop>
            <button class="ip-dropdown-trigger" @click="toggleDropdown('action', $event)">
              <span>{{ action }}</span>
              <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5">
                <path d="m6 9 6 6 6-6" />
              </svg>
            </button>
          </div>
        </div>
      </div>

      <div class="ip-field">
        <label class="ip-label">JSONL Data</label>
        <textarea
          v-model="jsonl"
          class="fg-input ip-textarea"
          rows="12"
          placeholder='{"id": "1", "title": "Hello World"}'
        />
        <span class="ip-hint">One JSON document per line</span>
      </div>

      <div class="ip-field">
        <label class="ip-label">Or upload a file</label>
        <label class="ip-file-btn">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 15V3" />
            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
            <path d="m7 10 5 5 5-5" />
          </svg>
          Choose file (.jsonl, .json, .txt)
          <input type="file" accept=".jsonl,.json,.txt" @change="onFile" class="ip-file-input" />
        </label>
      </div>

      <div class="ip-actions">
        <button
          class="fg-btn fg-btn--primary"
          :disabled="!collection || !jsonl || importing"
          @click="doImport"
        >
          {{ importing ? 'Importing...' : 'Import Documents' }}
        </button>
      </div>
    </div>

    <div v-if="result" class="ip-result">
      <div class="ip-result-header">Results</div>
      <pre class="ip-result-pre">{{ result }}</pre>
    </div>
  </div>

  <!-- Dropdown menus (teleported to escape overflow clipping) -->
  <Teleport to="body">
    <div
      v-if="openDropdown !== null"
      class="ip-dropdown-overlay"
      @mousedown="openDropdown = null"
    />
    <div
      v-if="openDropdown === 'collection'"
      class="ip-dropdown-menu"
      :style="{ top: dropdownPos.top + 'px', left: dropdownPos.left + 'px', width: dropdownPos.width + 'px' }"
    >
      <button
        v-for="c in connection.collectionNames"
        :key="c"
        class="ip-dropdown-option"
        :class="{ 'ip-dropdown-option--active': collection === c }"
        @click="selectCollection(c)"
      >{{ c }}</button>
      <div v-if="!connection.collectionNames.length" class="ip-dropdown-empty">No collections</div>
    </div>
    <div
      v-if="openDropdown === 'action'"
      class="ip-dropdown-menu"
      :style="{ top: dropdownPos.top + 'px', left: dropdownPos.left + 'px', width: dropdownPos.width + 'px' }"
    >
      <button
        v-for="a in ACTIONS"
        :key="a"
        class="ip-dropdown-option"
        :class="{ 'ip-dropdown-option--active': action === a }"
        @click="selectAction(a)"
      >{{ a }}</button>
    </div>
  </Teleport>
</template>

<style scoped>
.ip-root {
  padding: 20px;
  height: 100%;
  overflow-y: auto;
}

.ip-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 16px;
}

.ip-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--tv-text-primary);
}

/* Form */
.ip-form {
  background: var(--tv-bg-surface);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.ip-row--horizontal {
  display: flex;
  gap: 12px;
}

.ip-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  flex: 1;
  min-width: 0;
}

.ip-field--action {
  flex: 0 0 140px;
}

.ip-label {
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

/* Custom dropdown */
.ip-dropdown {
  position: relative;
}

.ip-dropdown-trigger {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
  height: 34px;
  padding: 0 10px 0 12px;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  background: var(--tv-bg-base);
  color: var(--tv-text-primary);
  font-size: var(--tv-font-size-sm);
  font-family: var(--tv-font-sans);
  cursor: pointer;
  transition: border-color 0.15s;
  box-sizing: border-box;
}
.ip-dropdown-trigger:hover {
  border-color: var(--tv-border-strong);
}

.ip-placeholder {
  color: var(--tv-text-muted);
}

.ip-textarea {
  width: 100%;
  box-sizing: border-box;
  height: auto;
  min-height: 200px;
  padding: 10px 12px;
  font-family: var(--tv-font-mono);
  font-size: var(--tv-font-size-xs);
  resize: vertical;
  line-height: 1.5;
}

.ip-hint {
  font-size: 11px;
  color: var(--tv-text-muted);
}

/* File upload button */
.ip-file-btn {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 14px;
  border: 1px dashed var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  background: var(--tv-bg-base);
  color: var(--tv-text-secondary);
  font-size: var(--tv-font-size-xs);
  cursor: pointer;
  transition: border-color 0.15s, background 0.15s, color 0.15s;
  width: fit-content;
}
.ip-file-btn:hover {
  border-color: var(--tv-border-strong);
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}
.ip-file-input {
  display: none;
}

/* Actions row */
.ip-actions {
  display: flex;
  justify-content: flex-end;
  padding-top: 4px;
}

/* Results */
.ip-result {
  margin-top: 16px;
  background: var(--tv-bg-surface);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  overflow: hidden;
}

.ip-result-header {
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  padding: 10px 16px;
  border-bottom: 1px solid var(--tv-border-default);
}

.ip-result-pre {
  padding: 12px 16px;
  margin: 0;
  font-family: var(--tv-font-mono);
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-primary);
  max-height: 300px;
  overflow: auto;
  white-space: pre-wrap;
  word-break: break-all;
  line-height: 1.5;
}
</style>

<style>
/* Teleported to body — cannot be scoped */
.ip-dropdown-overlay {
  position: fixed;
  inset: 0;
  z-index: 9999;
}

.ip-dropdown-menu {
  position: fixed;
  z-index: 10000;
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  box-shadow: var(--tv-shadow-lg);
  padding: 4px 0;
  max-height: 200px;
  overflow-y: auto;
}

.ip-dropdown-option {
  display: block;
  width: 100%;
  padding: 6px 12px;
  border: none;
  background: none;
  color: var(--tv-text-primary);
  font-size: var(--tv-font-size-sm);
  font-family: var(--tv-font-sans);
  text-align: left;
  cursor: pointer;
  transition: background 0.1s;
}
.ip-dropdown-option:hover {
  background: var(--tv-bg-hover);
}
.ip-dropdown-option--active {
  color: var(--tv-accent);
  font-weight: 600;
}

.ip-dropdown-empty {
  padding: 8px 12px;
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-muted);
}
</style>
