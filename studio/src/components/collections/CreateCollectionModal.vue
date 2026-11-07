<script setup lang="ts">
import { ref, nextTick } from 'vue'
import * as api from '../../utils/api'
import { useConnectionStore } from '../../stores/connection'

const emit = defineEmits<{ close: []; created: [name: string] }>()
const conn = useConnectionStore()

const FIELD_TYPES = [
  'string', 'int32', 'int64', 'float', 'bool',
  'string[]', 'int32[]', 'int64[]', 'float[]', 'geopoint',
]

interface FieldRow {
  name: string
  type: string
  facet: boolean
  optional: boolean
  sort: boolean
  index: boolean
}

const colName = ref('')
const indexMode = ref<'batch' | 'realtime'>('batch')
const fields = ref<FieldRow[]>([
  { name: '', type: 'string', facet: false, optional: false, sort: false, index: true },
])
const creating = ref(false)
const error = ref('')

// Custom dropdown state
const openDropdown = ref<number | null>(null)
const dropdownPos = ref({ top: 0, left: 0, width: 0 })

function toggleDropdown(i: number, e: MouseEvent) {
  if (openDropdown.value === i) {
    openDropdown.value = null
    return
  }
  const trigger = (e.currentTarget as HTMLElement)
  const rect = trigger.getBoundingClientRect()
  dropdownPos.value = { top: rect.bottom + 4, left: rect.left, width: rect.width }
  openDropdown.value = i
}

function selectType(i: number, type: string) {
  fields.value[i].type = type
  openDropdown.value = null
}

function closeDropdowns(e: MouseEvent) {
  const target = e.target as HTMLElement
  if (!target.closest('.ccm-type-dropdown')) {
    openDropdown.value = null
  }
}

function addField() {
  fields.value.push({ name: '', type: 'string', facet: false, optional: false, sort: false, index: true })
  nextTick(() => {
    const inputs = document.querySelectorAll<HTMLInputElement>('.ccm-field-name')
    inputs[inputs.length - 1]?.focus()
  })
}

function removeField(i: number) {
  if (fields.value.length > 1) fields.value.splice(i, 1)
}

function toggle(field: FieldRow, key: keyof FieldRow) {
  ;(field as any)[key] = !(field as any)[key]
}

async function create() {
  const name = colName.value.trim()
  if (!name) return
  const validFields = fields.value.filter(f => f.name.trim())
  if (validFields.length === 0) {
    error.value = 'At least one field is required'
    return
  }
  creating.value = true
  error.value = ''
  try {
    await api.createCollection({
      name,
      index_mode: indexMode.value,
      fields: validFields.map(f => ({
        name: f.name.trim(),
        type: f.type,
        facet: f.facet,
        optional: f.optional,
        sort: f.sort,
        index: f.index,
      })),
    })
    conn.setCurrentCollection(name)
    conn.fetchCollections()
    emit('created', name)
    emit('close')
  } catch (e: any) {
    error.value = e.message || 'Failed to create collection'
  } finally {
    creating.value = false
  }
}
</script>

<template>
  <Teleport to="body">
    <Transition name="modal">
    <div class="fg-modal-overlay" @mousedown.self="emit('close')" @click="closeDropdowns">
      <div class="ccm-panel" role="dialog" aria-modal="true" aria-label="Create Collection">
        <div class="ccm-header">
          <span class="ccm-title">New Collection</span>
          <button class="ccm-close" aria-label="Close" @click="emit('close')">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M18 6 6 18" /><path d="m6 6 12 12" />
            </svg>
          </button>
        </div>

        <div class="ccm-body">
          <!-- Name -->
          <div class="ccm-section">
            <label class="ccm-label">Name</label>
            <input
              v-model="colName"
              class="fg-input ccm-name-input"
              placeholder="my_collection"
              @keydown.enter="create"
            />
          </div>

          <!-- Index Mode -->
          <div class="ccm-section">
            <label class="ccm-label">Index Mode</label>
            <div class="ccm-mode-toggle">
              <button
                class="ccm-mode-btn"
                :class="{ 'ccm-mode-btn--active': indexMode === 'batch' }"
                @click="indexMode = 'batch'"
              >Batch</button>
              <button
                class="ccm-mode-btn"
                :class="{ 'ccm-mode-btn--active': indexMode === 'realtime' }"
                @click="indexMode = 'realtime'"
              >Realtime</button>
            </div>
            <div class="ccm-mode-desc">
              {{ indexMode === 'batch'
                ? 'Batch: full rebuild via TCP ingest. Best for bulk data loads.'
                : 'Realtime: instant upsert/delete via HTTP. Each write is a small segment, on disk in a write-ahead log before it is acknowledged; segments merge in the background.' }}
            </div>
          </div>

          <!-- Fields -->
          <div class="ccm-section">
            <div class="ccm-label">Fields</div>

            <div class="ccm-fields-header">
              <span class="ccm-fh-name">Name</span>
              <span class="ccm-fh-type">Type</span>
              <span class="ccm-fh-flag">Facet</span>
              <span class="ccm-fh-flag">Sort</span>
              <span class="ccm-fh-flag">Index</span>
              <span class="ccm-fh-flag">Optional</span>
              <span class="ccm-fh-rm" />
            </div>

            <div v-for="(f, i) in fields" :key="i" class="ccm-field-row">
              <input
                v-model="f.name"
                class="fg-input ccm-field-name"
                placeholder="field_name"
                @keydown.enter="create"
              />

              <!-- Custom type dropdown -->
              <div class="ccm-type-dropdown" @click.stop>
                <button class="ccm-type-trigger" @click="toggleDropdown(i, $event)">
                  <span>{{ f.type }}</span>
                  <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5">
                    <path d="m6 9 6 6 6-6" />
                  </svg>
                </button>
              </div>

              <!-- Custom checkboxes -->
              <label class="fg-checkbox" title="Facet">
                <input type="checkbox" :checked="f.facet" @change="toggle(f, 'facet')" />
                <span class="fg-checkbox__box" />
              </label>
              <label class="fg-checkbox" title="Sort">
                <input type="checkbox" :checked="f.sort" @change="toggle(f, 'sort')" />
                <span class="fg-checkbox__box" />
              </label>
              <label class="fg-checkbox" title="Index">
                <input type="checkbox" :checked="f.index" @change="toggle(f, 'index')" />
                <span class="fg-checkbox__box" />
              </label>
              <label class="fg-checkbox" title="Optional">
                <input type="checkbox" :checked="f.optional" @change="toggle(f, 'optional')" />
                <span class="fg-checkbox__box" />
              </label>

              <button
                class="ccm-rm-btn"
                :disabled="fields.length <= 1"
                title="Remove field"
                @click="removeField(i)"
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M18 6 6 18" /><path d="m6 6 12 12" />
                </svg>
              </button>
            </div>

            <button class="ccm-add-btn" @click="addField">
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M5 12h14" /><path d="M12 5v14" />
              </svg>
              Add field
            </button>
          </div>

          <div v-if="error" class="ccm-error">{{ error }}</div>
        </div>

        <div class="ccm-footer">
          <button class="fg-btn" @click="emit('close')">Cancel</button>
          <button
            class="fg-btn fg-btn--primary"
            :disabled="creating || !colName.trim()"
            @click="create"
          >
            {{ creating ? 'Creating...' : 'Create Collection' }}
          </button>
        </div>
      </div>
    </div>
    </Transition>
  </Teleport>

  <!-- Type dropdown menu (teleported to escape overflow clipping) -->
  <Teleport to="body">
    <div
      v-if="openDropdown !== null"
      class="ccm-type-overlay"
      @mousedown="openDropdown = null"
    />
    <div
      v-if="openDropdown !== null"
      class="ccm-type-menu"
      :style="{ top: dropdownPos.top + 'px', left: dropdownPos.left + 'px', width: dropdownPos.width + 'px' }"
    >
      <button
        v-for="t in FIELD_TYPES"
        :key="t"
        class="ccm-type-option"
        :class="{ 'ccm-type-option--active': fields[openDropdown!].type === t }"
        @click="selectType(openDropdown!, t)"
      >{{ t }}</button>
    </div>
  </Teleport>
</template>

<style scoped>
.ccm-panel {
  width: 600px;
  max-height: 80vh;
  display: flex;
  flex-direction: column;
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-lg);
  box-shadow: var(--tv-shadow-lg);
}

.ccm-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 20px 12px;
  border-bottom: 1px solid var(--tv-border-default);
  flex-shrink: 0;
}

.ccm-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--tv-text-primary);
}

.ccm-close {
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
.ccm-close:hover {
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}

.ccm-body {
  padding: 16px 20px;
  overflow-y: auto;
  flex: 1;
}

.ccm-section {
  margin-bottom: 16px;
}
.ccm-section:last-child {
  margin-bottom: 0;
}

.ccm-label {
  display: block;
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 6px;
}

.ccm-name-input {
  width: 100%;
  box-sizing: border-box;
}

/* Fields table */
.ccm-fields-header {
  display: grid;
  grid-template-columns: 1fr 100px 46px 36px 40px 58px 28px;
  gap: 6px;
  padding: 0 0 4px;
  font-size: 11px;
  font-weight: 500;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.03em;
}

.ccm-fh-flag {
  text-align: center;
}

.ccm-field-row {
  display: grid;
  grid-template-columns: 1fr 100px 46px 36px 40px 58px 28px;
  gap: 6px;
  align-items: center;
  margin-bottom: 4px;
}

.ccm-field-name {
  height: 30px;
  font-size: var(--tv-font-size-xs);
}

/* Custom type dropdown */
.ccm-type-dropdown {
  position: relative;
}

.ccm-type-trigger {
  display: flex;
  align-items: center;
  justify-content: space-between;
  width: 100%;
  height: 30px;
  padding: 0 6px 0 8px;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  background: var(--tv-bg-base);
  color: var(--tv-text-primary);
  font-size: var(--tv-font-size-xs);
  font-family: var(--tv-font-mono);
  cursor: pointer;
  transition: border-color 0.15s;
}
.ccm-type-trigger:hover {
  border-color: var(--tv-border-strong);
}


/* Checkbox alignment */
.ccm-field-row .fg-checkbox {
  justify-self: center;
}

.ccm-rm-btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: none;
  border-radius: 4px;
  background: none;
  color: var(--tv-text-muted);
  cursor: pointer;
  transition: color 0.15s, background 0.15s;
}
.ccm-rm-btn:hover {
  color: var(--tv-status-error);
  background: var(--tv-bg-hover);
}
.ccm-rm-btn:disabled {
  opacity: 0.3;
  cursor: not-allowed;
}

.ccm-add-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin-top: 4px;
  padding: 4px 8px;
  border: none;
  border-radius: var(--tv-radius-sm);
  background: none;
  color: var(--tv-text-secondary);
  font-size: var(--tv-font-size-xs);
  font-family: inherit;
  cursor: pointer;
  transition: color 0.15s, background 0.15s;
}
.ccm-add-btn:hover {
  color: var(--tv-text-primary);
  background: var(--tv-bg-hover);
}

/* Index mode toggle */
.ccm-mode-toggle {
  display: flex;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  overflow: hidden;
  width: fit-content;
}

.ccm-mode-btn {
  padding: 5px 16px;
  border: none;
  background: none;
  color: var(--tv-text-secondary);
  font-size: var(--tv-font-size-xs);
  font-family: inherit;
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.ccm-mode-btn + .ccm-mode-btn {
  border-left: 1px solid var(--tv-border-default);
}
.ccm-mode-btn--active {
  background: var(--tv-accent);
  color: var(--tv-text-inverse);
}

.ccm-mode-desc {
  font-size: 11px;
  color: var(--tv-text-muted);
  margin-top: 6px;
  line-height: 1.4;
}

.ccm-error {
  font-size: 12px;
  color: var(--tv-status-error);
  margin-top: 8px;
}

.ccm-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 20px;
  border-top: 1px solid var(--tv-border-default);
  flex-shrink: 0;
}
</style>

<style>
/* Teleported to body — cannot be scoped */
.ccm-type-overlay {
  position: fixed;
  inset: 0;
  z-index: 9999;
}

.ccm-type-menu {
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

.ccm-type-option {
  display: block;
  width: 100%;
  padding: 5px 10px;
  border: none;
  background: none;
  color: var(--tv-text-primary);
  font-size: var(--tv-font-size-xs);
  font-family: var(--tv-font-mono);
  text-align: left;
  cursor: pointer;
  transition: background 0.1s;
}
.ccm-type-option:hover {
  background: var(--tv-bg-hover);
}
.ccm-type-option--active {
  color: var(--tv-accent);
  font-weight: 600;
}
</style>
