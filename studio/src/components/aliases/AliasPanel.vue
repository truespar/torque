<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useConnectionStore } from '../../stores/connection'
import * as api from '../../utils/api'

const emit = defineEmits<{ close: [] }>()

const conn = useConnectionStore()

interface AliasEntry {
  name: string
  collection_name: string
}

const aliases = ref<AliasEntry[]>([])
const loading = ref(false)
const error = ref('')

// Create form
const newAlias = ref('')
const newTarget = ref('')
const creating = ref(false)

// Edit state
const editingAlias = ref<string | null>(null)
const editTarget = ref('')
const saving = ref(false)

// Delete state (two-click)
const confirmingDelete = ref<string | null>(null)
const deleting = ref(false)

// Custom dropdown state
const showEditDropdown = ref(false)
const showCreateDropdown = ref(false)
const dropdownPos = ref({ top: 0, left: 0, width: 0 })

function openDropdownAt(el: HTMLElement) {
  const rect = el.getBoundingClientRect()
  dropdownPos.value = { top: rect.bottom + 4, left: rect.left, width: rect.width }
}

function toggleEditDropdown(e: MouseEvent) {
  if (showEditDropdown.value) {
    showEditDropdown.value = false
    return
  }
  showCreateDropdown.value = false
  openDropdownAt(e.currentTarget as HTMLElement)
  showEditDropdown.value = true
}

function toggleCreateDropdown(e: MouseEvent) {
  if (showCreateDropdown.value) {
    showCreateDropdown.value = false
    return
  }
  showEditDropdown.value = false
  openDropdownAt(e.currentTarget as HTMLElement)
  showCreateDropdown.value = true
}

function selectEditTarget(name: string) {
  editTarget.value = name
  showEditDropdown.value = false
}

function selectCreateTarget(name: string) {
  newTarget.value = name
  showCreateDropdown.value = false
}

function closeAllDropdowns() {
  showEditDropdown.value = false
  showCreateDropdown.value = false
}

async function loadAliases() {
  loading.value = true
  error.value = ''
  try {
    const resp = await api.aliases()
    aliases.value = resp.aliases || []
  } catch (e: any) {
    error.value = e.message || 'Failed to load aliases'
  } finally {
    loading.value = false
  }
}

async function createAlias() {
  const alias = newAlias.value.trim()
  const collection = newTarget.value
  if (!alias || !collection) return
  if (conn.collectionList.some(c => c.name === alias)) {
    error.value = `"${alias}" already exists as a collection — alias names cannot shadow collection names`
    return
  }
  creating.value = true
  error.value = ''
  try {
    await api.createAlias(alias, { collection_name: collection })
    newAlias.value = ''
    newTarget.value = ''
    await refresh()
  } catch (e: any) {
    error.value = e.message || 'Failed to create alias'
  } finally {
    creating.value = false
  }
}

function startEdit(entry: AliasEntry) {
  editingAlias.value = entry.name
  editTarget.value = entry.collection_name
}

function cancelEdit() {
  editingAlias.value = null
}

async function saveEdit() {
  if (!editingAlias.value) return
  saving.value = true
  error.value = ''
  try {
    await api.createAlias(editingAlias.value, { collection_name: editTarget.value })
    editingAlias.value = null
    await refresh()
  } catch (e: any) {
    error.value = e.message || 'Failed to update alias'
  } finally {
    saving.value = false
  }
}

async function removeAlias(alias: string) {
  if (confirmingDelete.value !== alias) {
    confirmingDelete.value = alias
    return
  }
  deleting.value = true
  error.value = ''
  try {
    await api.deleteAlias(alias)
    confirmingDelete.value = null
    await refresh()
  } catch (e: any) {
    error.value = e.message || 'Failed to delete alias'
  } finally {
    deleting.value = false
  }
}

function cancelConfirm() {
  confirmingDelete.value = null
}

async function refresh() {
  await Promise.all([loadAliases(), conn.fetchCollections()])
}

onMounted(loadAliases)
</script>

<template>
  <Teleport to="body">
    <Transition name="modal">
    <div class="fg-modal-overlay" @mousedown.self="emit('close')" @click="closeAllDropdowns">
      <div class="ap-panel" role="dialog" aria-modal="true" aria-label="Collection Aliases">
        <div class="ap-header">
          <span class="ap-title">Collection Aliases</span>
          <button class="ap-close" aria-label="Close" @click="emit('close')">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M18 6 6 18" /><path d="m6 6 12 12" />
            </svg>
          </button>
        </div>

        <!-- Alias list -->
        <div class="ap-section">
          <div class="ap-section-title">
            Aliases
            <span v-if="aliases.length" class="ap-count">{{ aliases.length }}</span>
          </div>

          <div v-if="loading" class="ap-loading">
            <div class="ap-spinner" />
            <span>Loading aliases...</span>
          </div>

          <div v-else-if="aliases.length === 0" class="ap-empty">
            No aliases defined
          </div>

          <div v-else class="ap-list">
            <div
              v-for="entry in aliases"
              :key="entry.name"
              class="ap-item"
              @mouseleave="entry.name === confirmingDelete ? cancelConfirm() : null"
            >
              <!-- View mode -->
              <template v-if="editingAlias !== entry.name">
                <div class="ap-item-info">
                  <span class="ap-alias-name">{{ entry.name }}</span>
                  <span class="ap-arrow">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                      <path d="M5 12h14" /><path d="m12 5 7 7-7 7" />
                    </svg>
                  </span>
                  <span class="ap-target-name">{{ entry.collection_name }}</span>
                </div>
                <div class="ap-item-actions">
                  <button class="ap-edit-btn" title="Edit" @click="startEdit(entry)">
                    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                      <path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" /><path d="m15 5 4 4" />
                    </svg>
                  </button>
                  <button
                    class="ap-delete-btn"
                    :class="{ 'ap-delete-btn--confirm': confirmingDelete === entry.name }"
                    :disabled="deleting"
                    @click="removeAlias(entry.name)"
                  >
                    {{ confirmingDelete === entry.name ? 'Confirm?' : 'Delete' }}
                  </button>
                </div>
              </template>

              <!-- Edit mode -->
              <template v-else>
                <div class="ap-edit-row">
                  <span class="ap-alias-name">{{ entry.name }}</span>
                  <span class="ap-arrow">
                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                      <path d="M5 12h14" /><path d="m12 5 7 7-7 7" />
                    </svg>
                  </span>
                  <div class="ap-dropdown" @click.stop>
                    <button class="ap-dropdown-trigger" @click="toggleEditDropdown($event)">
                      <span>{{ editTarget || 'Select...' }}</span>
                      <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="m6 9 6 6 6-6" /></svg>
                    </button>
                  </div>
                </div>
                <div class="ap-item-actions">
                  <button class="ap-save-btn" :disabled="saving" @click="saveEdit">Save</button>
                  <button class="ap-cancel-btn" @click="cancelEdit">Cancel</button>
                </div>
              </template>
            </div>
          </div>
        </div>

        <!-- Create form -->
        <div class="ap-section">
          <div class="ap-section-title">Create Alias</div>
          <div class="ap-create-form">
            <input
              v-model="newAlias"
              class="fg-input"
              placeholder="Alias name..."
              @keydown.enter="createAlias"
            />
            <div class="ap-dropdown" @click.stop>
              <button class="ap-dropdown-trigger" @click="toggleCreateDropdown($event)">
                <span :class="{ 'ap-dropdown-placeholder': !newTarget }">{{ newTarget || 'Target collection...' }}</span>
                <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="m6 9 6 6 6-6" /></svg>
              </button>
            </div>
            <button
              class="ap-create-btn"
              :disabled="creating || !newAlias.trim() || !newTarget"
              @click="createAlias"
            >
              {{ creating ? 'Creating...' : 'Create' }}
            </button>
          </div>
        </div>

        <div v-if="error" class="ap-error">{{ error }}</div>
      </div>
    </div>
    </Transition>
  </Teleport>

  <!-- Dropdown menus (teleported to escape overflow clipping) -->
  <Teleport to="body">
    <div
      v-if="showEditDropdown || showCreateDropdown"
      class="ap-dd-overlay"
      @mousedown="closeAllDropdowns"
    />
    <!-- Edit target dropdown -->
    <div
      v-if="showEditDropdown"
      class="ap-dd-menu"
      :style="{ top: dropdownPos.top + 'px', left: dropdownPos.left + 'px', width: dropdownPos.width + 'px' }"
    >
      <button
        v-for="col in conn.collectionList"
        :key="col.name"
        class="ap-dd-option"
        :class="{ 'ap-dd-option--active': editTarget === col.name }"
        @click="selectEditTarget(col.name)"
      >{{ col.name }}</button>
    </div>
    <!-- Create target dropdown -->
    <div
      v-if="showCreateDropdown"
      class="ap-dd-menu"
      :style="{ top: dropdownPos.top + 'px', left: dropdownPos.left + 'px', width: dropdownPos.width + 'px' }"
    >
      <button
        v-for="col in conn.collectionList"
        :key="col.name"
        class="ap-dd-option"
        :class="{ 'ap-dd-option--active': newTarget === col.name }"
        @click="selectCreateTarget(col.name)"
      >{{ col.name }}</button>
    </div>
  </Teleport>
</template>

<style scoped>
.ap-panel {
  width: 360px;
  max-height: 80vh;
  overflow-y: auto;
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-lg);
  box-shadow: var(--tv-shadow-lg);
}

.ap-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 20px 12px;
  border-bottom: 1px solid var(--tv-border-default);
}

.ap-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--tv-text-primary);
}

.ap-close {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: none;
  border-radius: var(--tv-radius-sm);
  background: none;
  color: var(--tv-text-muted);
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}

.ap-close:hover {
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}

.ap-section {
  padding: 12px 20px;
  border-bottom: 1px solid var(--tv-border-default);
}

.ap-section:last-child {
  border-bottom: none;
}

.ap-section-title {
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 8px;
  display: flex;
  align-items: baseline;
  justify-content: space-between;
}

.ap-count {
  font-weight: 400;
  text-transform: none;
  letter-spacing: normal;
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-muted);
}

.ap-loading {
  display: flex;
  align-items: center;
  gap: 8px;
  color: var(--tv-text-secondary);
  font-size: 12px;
  padding: 4px 0;
}

.ap-spinner {
  width: 14px;
  height: 14px;
  border: 2px solid var(--tv-border-default);
  border-top-color: var(--tv-accent);
  border-radius: 50%;
  animation: ap-spin 0.8s linear infinite;
}

@keyframes ap-spin {
  to { transform: rotate(360deg); }
}

.ap-empty {
  color: var(--tv-text-muted);
  font-size: 12px;
  padding: 4px 0;
}

.ap-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.ap-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 6px 8px;
  border-radius: var(--tv-radius-sm);
  background: var(--tv-bg-surface);
}

.ap-item-info {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
  flex: 1;
}

.ap-alias-name {
  font-size: var(--tv-font-size-sm);
  font-weight: 500;
  color: var(--tv-text-primary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.ap-arrow {
  flex-shrink: 0;
  color: var(--tv-text-muted);
  display: flex;
  align-items: center;
}

.ap-target-name {
  font-size: var(--tv-font-size-sm);
  color: var(--tv-text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.ap-item-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-shrink: 0;
}

.ap-edit-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  border: none;
  border-radius: 4px;
  background: none;
  color: var(--tv-text-muted);
  cursor: pointer;
  transition: color 0.15s, background 0.15s;
}

.ap-edit-btn:hover {
  color: var(--tv-text-primary);
  background: var(--tv-bg-hover);
}

.ap-delete-btn {
  flex-shrink: 0;
  padding: 3px 8px;
  font-size: 11px;
  font-weight: 500;
  color: var(--tv-text-muted);
  background: none;
  border: 1px solid var(--tv-border-default);
  border-radius: 4px;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}

.ap-delete-btn:hover {
  color: var(--tv-status-error);
  border-color: var(--tv-status-error);
}

.ap-delete-btn--confirm {
  color: var(--tv-text-inverse);
  background: var(--tv-status-error);
  border-color: var(--tv-status-error);
}

.ap-delete-btn--confirm:hover {
  color: var(--tv-text-inverse);
}

.ap-delete-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

/* Edit mode */
.ap-edit-row {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: 1;
  min-width: 0;
}

.ap-save-btn {
  padding: 3px 8px;
  font-size: 11px;
  font-weight: 500;
  color: var(--tv-text-inverse);
  background: var(--tv-accent);
  border: none;
  border-radius: 4px;
  cursor: pointer;
  transition: background 0.15s;
}

.ap-save-btn:hover {
  background: var(--tv-accent-hover);
}

.ap-save-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.ap-cancel-btn {
  padding: 3px 8px;
  font-size: 11px;
  font-weight: 500;
  color: var(--tv-text-muted);
  background: none;
  border: 1px solid var(--tv-border-default);
  border-radius: 4px;
  cursor: pointer;
  transition: color 0.15s, background 0.15s;
}

.ap-cancel-btn:hover {
  color: var(--tv-text-primary);
  background: var(--tv-bg-hover);
}

/* Create form */
.ap-create-form {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.ap-create-btn {
  align-self: flex-end;
  padding: 5px 14px;
  font-size: 12px;
  font-weight: 500;
  color: var(--tv-text-inverse);
  background: var(--tv-accent);
  border: none;
  border-radius: var(--tv-radius-sm);
  cursor: pointer;
  transition: background 0.15s;
}

.ap-create-btn:hover {
  background: var(--tv-accent-hover);
}

.ap-create-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

/* Custom dropdown */
.ap-dropdown {
  position: relative;
  flex: 1;
  min-width: 0;
}

.ap-dropdown-trigger {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
  height: 34px;
  padding: 0 8px 0 10px;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  background: var(--tv-bg-base);
  color: var(--tv-text-primary);
  font-size: var(--tv-font-size-xs);
  font-family: var(--tv-font-sans);
  cursor: pointer;
  transition: border-color 0.15s;
  text-align: left;
}
.ap-dropdown-trigger:hover {
  border-color: var(--tv-border-strong);
}

.ap-dropdown-placeholder {
  color: var(--tv-text-muted);
}


.ap-error {
  padding: 8px 20px 12px;
  font-size: 12px;
  color: var(--tv-status-error);
}
</style>

<style>
/* Teleported to body — cannot be scoped */
.ap-create-form .fg-input {
  width: 100%;
  box-sizing: border-box;
}

.ap-dd-overlay {
  position: fixed;
  inset: 0;
  z-index: 9999;
}

.ap-dd-menu {
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

.ap-dd-option {
  display: block;
  width: 100%;
  padding: 6px 10px;
  border: none;
  background: none;
  color: var(--tv-text-primary);
  font-size: var(--tv-font-size-xs);
  font-family: var(--tv-font-sans);
  text-align: left;
  cursor: pointer;
  transition: background 0.1s;
}
.ap-dd-option:hover {
  background: var(--tv-bg-hover);
}
.ap-dd-option--active {
  color: var(--tv-accent);
  font-weight: 600;
}
</style>
