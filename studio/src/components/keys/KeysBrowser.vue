<script setup lang="ts">
import { ref, onMounted, reactive } from 'vue'
import * as api from '@/utils/api'
import { useToast } from '@/composables/useToast'

const toast = useToast()
const keyList = ref<any[]>([])
const showCreate = ref(false)
const createdKeyValue = ref('')
const newKey = reactive({ description: '', actions: '*', collections: '*' })

// Delete key modal state
const deletingKeyTarget = ref<{ id: number; prefix: string } | null>(null)
const deletingKey = ref(false)

async function fetchKeys() {
  try { keyList.value = (await api.keys()).keys || [] } catch {}
}

onMounted(fetchKeys)

function openCreate() {
  newKey.description = ''
  newKey.actions = '*'
  newKey.collections = '*'
  createdKeyValue.value = ''
  showCreate.value = true
}

function closeCreate() {
  showCreate.value = false
  createdKeyValue.value = ''
}

async function create() {
  try {
    const actions = newKey.actions === '*' ? ['*'] : newKey.actions.split(',').map(s => s.trim())
    const collections = newKey.collections === '*' ? ['*'] : newKey.collections.split(',').map(s => s.trim())
    const resp = await api.createKey({ description: newKey.description, actions, collections })
    createdKeyValue.value = resp.value || ''
    toast.success('API key created')
    fetchKeys()
  } catch (e: any) {
    toast.error(e.message || 'Failed to create key')
  }
}

function openDeleteKey(id: number, prefix: string) {
  deletingKeyTarget.value = { id, prefix }
}

function closeDeleteKey() {
  deletingKeyTarget.value = null
}

async function confirmDeleteKey() {
  const t = deletingKeyTarget.value
  if (!t) return
  deletingKey.value = true
  try {
    await api.deleteKey(String(t.id))
    toast.success('API key deleted')
    deletingKeyTarget.value = null
    fetchKeys()
  } catch (e: any) {
    toast.error(e.message || 'Failed to delete key')
  } finally {
    deletingKey.value = false
  }
}
</script>

<template>
  <div class="kb-root">
    <div class="kb-header">
      <span class="kb-title">API Keys</span>
      <button class="fg-btn fg-btn--primary fg-btn--sm" @click="openCreate">
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M5 12h14" /><path d="M12 5v14" />
        </svg>
        Create Key
      </button>
    </div>

    <div v-if="keyList.length" class="kb-list">
      <div v-for="k in keyList" :key="k.id" class="kb-key">
        <div class="kb-key-main">
          <span class="kb-key-prefix">{{ k.value_prefix }}...</span>
          <span v-if="k.description" class="kb-key-desc">{{ k.description }}</span>
        </div>
        <div class="kb-key-meta">
          <span class="kb-badge">{{ k.actions?.join(', ') || '*' }}</span>
          <span class="kb-badge">{{ k.collections?.join(', ') || '*' }}</span>
          <span class="kb-key-expires">{{ k.expires_at ? new Date(k.expires_at * 1000).toLocaleDateString() : 'No expiry' }}</span>
        </div>
        <button
          class="kb-key-delete"
          @click="openDeleteKey(k.id, k.value_prefix)"
          title="Delete key"
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M18 6 6 18" /><path d="m6 6 12 12" />
          </svg>
        </button>
      </div>
    </div>

    <div v-else class="kb-empty">No API keys</div>
  </div>

  <!-- Create key modal -->
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="showCreate" class="fg-modal-overlay" @mousedown.self="closeCreate">
        <div class="kb-panel" role="dialog" aria-modal="true" aria-label="Create API Key">
          <div class="kb-panel-header">
            <span class="kb-panel-title">Create API Key</span>
            <button class="kb-panel-close" aria-label="Close" @click="closeCreate">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 6 6 18" /><path d="m6 6 12 12" />
              </svg>
            </button>
          </div>
          <div class="kb-panel-body">
            <div class="kb-form-row">
              <label class="kb-form-label">Description</label>
              <input v-model="newKey.description" class="fg-input kb-form-input" placeholder="My search key" />
            </div>
            <div class="kb-form-row">
              <label class="kb-form-label">Actions</label>
              <input v-model="newKey.actions" class="fg-input kb-form-input" placeholder="documents:search, documents:get" />
              <span class="kb-form-hint">Comma-separated, or * for all</span>
            </div>
            <div class="kb-form-row">
              <label class="kb-form-label">Collections</label>
              <input v-model="newKey.collections" class="fg-input kb-form-input" placeholder="*" />
              <span class="kb-form-hint">Comma-separated, or * for all</span>
            </div>

            <div v-if="createdKeyValue" class="kb-created">
              <div class="kb-created-label">Key created — copy it now, it won't be shown again.</div>
              <code class="kb-created-value">{{ createdKeyValue }}</code>
            </div>
          </div>
          <div class="kb-panel-footer">
            <button class="fg-btn" @click="closeCreate">{{ createdKeyValue ? 'Done' : 'Cancel' }}</button>
            <button v-if="!createdKeyValue" class="fg-btn fg-btn--primary" @click="create">Create Key</button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>

  <!-- Delete key confirmation modal -->
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="deletingKeyTarget" class="fg-modal-overlay" @mousedown.self="closeDeleteKey">
        <div class="kb-panel" role="dialog" aria-modal="true" aria-label="Delete API Key">
          <div class="kb-panel-header">
            <div>
              <span class="kb-panel-title">Delete API Key</span>
              <span class="kb-panel-subtitle">{{ deletingKeyTarget.prefix }}...</span>
            </div>
            <button class="kb-panel-close" aria-label="Close" @click="closeDeleteKey">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 6 6 18" /><path d="m6 6 12 12" />
              </svg>
            </button>
          </div>
          <div class="kb-panel-body">
            <div class="kb-warning">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" />
                <path d="M12 9v4" /><path d="M12 17h.01" />
              </svg>
              <span>This will permanently revoke this API key. Any applications using it will lose access immediately. This cannot be undone.</span>
            </div>
          </div>
          <div class="kb-panel-footer">
            <button class="fg-btn" @click="closeDeleteKey">Cancel</button>
            <button class="fg-btn fg-btn--danger" :disabled="deletingKey" @click="confirmDeleteKey">
              {{ deletingKey ? 'Deleting...' : 'Delete Key' }}
            </button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.kb-root {
  padding: 20px;
  height: 100%;
  overflow-y: auto;
}

.kb-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 16px;
}

.kb-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--tv-text-primary);
}

.kb-empty {
  color: var(--tv-text-muted);
  font-size: var(--tv-font-size-sm);
  padding: 20px 0;
}

/* Key list */
.kb-list {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.kb-key {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  background: var(--tv-bg-surface);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
}

.kb-key-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1;
  min-width: 0;
}

.kb-key-prefix {
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  font-family: var(--tv-font-mono);
  color: var(--tv-text-primary);
}

.kb-key-desc {
  font-size: 11px;
  color: var(--tv-text-muted);
}

.kb-key-meta {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.kb-badge {
  font-size: 11px;
  color: var(--tv-text-muted);
  padding: 1px 6px;
  border-radius: 3px;
  background: var(--tv-bg-inset);
  font-family: var(--tv-font-mono);
}

.kb-key-expires {
  font-size: 11px;
  color: var(--tv-text-muted);
  white-space: nowrap;
}

.kb-key-delete {
  display: flex;
  align-items: center;
  justify-content: center;
  min-width: 20px;
  height: 20px;
  border: none;
  border-radius: 3px;
  background: none;
  color: var(--tv-text-muted);
  cursor: pointer;
  opacity: 0;
  transition: opacity 0.1s, color 0.1s, background 0.1s;
  padding: 0 4px;
}
.kb-key:hover .kb-key-delete {
  opacity: 1;
}
.kb-key-delete:hover {
  color: var(--tv-status-error);
  background: var(--tv-bg-hover);
}

/* Modal panel */
.kb-panel {
  width: 420px;
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-lg);
  box-shadow: var(--tv-shadow-lg);
}

.kb-panel-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  padding: 16px 20px 12px;
  border-bottom: 1px solid var(--tv-border-default);
}

.kb-panel-title {
  display: block;
  font-size: 14px;
  font-weight: 600;
  color: var(--tv-text-primary);
}

.kb-panel-subtitle {
  display: block;
  font-size: var(--tv-font-size-xs);
  font-family: var(--tv-font-mono);
  color: var(--tv-text-secondary);
  margin-top: 2px;
}

.kb-panel-close {
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
  flex-shrink: 0;
}
.kb-panel-close:hover {
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}

.kb-panel-body {
  padding: 16px 20px;
}

.kb-panel-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 20px;
  border-top: 1px solid var(--tv-border-default);
}

/* Form rows */
.kb-form-row {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: 12px;
}
.kb-form-row:last-child {
  margin-bottom: 0;
}

.kb-form-label {
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.kb-form-input {
  width: 100%;
  box-sizing: border-box;
}

.kb-form-hint {
  font-size: 11px;
  color: var(--tv-text-muted);
}

/* Created key display */
.kb-created {
  margin-top: 16px;
  padding: 12px;
  border-radius: var(--tv-radius-md);
  background: color-mix(in srgb, var(--tv-status-success) 10%, transparent);
  border: 1px solid color-mix(in srgb, var(--tv-status-success) 30%, transparent);
}

.kb-created-label {
  font-size: var(--tv-font-size-xs);
  color: var(--tv-status-success);
  font-weight: 600;
  margin-bottom: 8px;
}

.kb-created-value {
  font-size: var(--tv-font-size-sm);
  font-family: var(--tv-font-mono);
  color: var(--tv-text-primary);
  user-select: all;
  word-break: break-all;
}

/* Warning */
.kb-warning {
  display: flex;
  gap: 8px;
  padding: 10px 12px;
  border-radius: var(--tv-radius-md);
  background: color-mix(in srgb, var(--tv-status-error) 10%, transparent);
  border: 1px solid color-mix(in srgb, var(--tv-status-error) 30%, transparent);
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-secondary);
  line-height: 1.5;
}
.kb-warning svg {
  flex-shrink: 0;
  color: var(--tv-status-error);
  margin-top: 1px;
}

:deep(.fg-btn--danger) {
  background: var(--tv-status-error);
  color: var(--tv-text-inverse);
  border-color: var(--tv-status-error);
}
:deep(.fg-btn--danger:hover) {
  background: color-mix(in srgb, var(--tv-status-error) 85%, black);
}
:deep(.fg-btn--danger:disabled) {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
