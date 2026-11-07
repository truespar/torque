<script setup lang="ts">
import { ref, computed, onMounted, reactive, watch } from 'vue'
import { useSchemaStore } from '../../stores/schema'
import { useConnectionStore } from '../../stores/connection'
import { useToast } from '../../composables/useToast'
import * as api from '../../utils/api'
import CreateCollectionModal from '../collections/CreateCollectionModal.vue'

const FIELD_TYPES = [
  'string', 'int32', 'int64', 'float', 'bool',
  'string[]', 'int32[]', 'int64[]', 'float[]', 'geopoint',
]

const schema = useSchemaStore()
const conn = useConnectionStore()
const toast = useToast()
const showCreate = ref(false)

const currentCol = computed(() =>
  schema.collections.find((c: any) => c.name === conn.currentCollection)
)

const currentFields = computed(() =>
  (currentCol.value?.fields ?? []).filter((f: any) => f.name)
)

interface FieldNode {
  field: any | null      // null for synthetic parent nodes
  name: string           // display name (leaf part)
  fullName: string       // full dotted name
  children: FieldNode[]
}

const fieldTree = computed((): FieldNode[] => {
  const fields = currentFields.value
  const groups = new Map<string, any[]>()
  const topLevel: any[] = []

  for (const f of fields) {
    const dot = f.name.indexOf('.')
    if (dot === -1) {
      topLevel.push(f)
    } else {
      const prefix = f.name.substring(0, dot)
      if (!groups.has(prefix)) groups.set(prefix, [])
      groups.get(prefix)!.push(f)
    }
  }

  const result: FieldNode[] = []

  // Interleave: for each top-level field, emit it, then if a group matches, emit the group
  const usedGroups = new Set<string>()
  for (const f of topLevel) {
    result.push({ field: f, name: f.name, fullName: f.name, children: [] })
    // Check if there's a nested group with this prefix (unlikely but possible)
    if (groups.has(f.name)) {
      usedGroups.add(f.name)
      // Don't emit as separate group, already handled
    }
  }

  // Emit remaining nested groups (parent is not a real field)
  for (const [prefix, children] of groups) {
    const isArray = children.some((c: any) => c.type?.endsWith('[]'))
    result.push({
      field: null,
      name: prefix,
      fullName: prefix,
      children: children.map((c: any) => ({
        field: c,
        name: c.name.substring(prefix.length + 1),
        fullName: c.name,
        children: [],
      })),
    })
  }

  return result
})

const expandedGroups = reactive(new Set<string>())

// Delete collection modal state
const deletingColTarget = ref<string | null>(null)
const deleteConfirmInput = ref('')
const deletingCol = ref(false)
const showDropDisabledModal = ref(false)

// Drop field modal state
const droppingFieldTarget = ref<{ colName: string; fieldName: string; fieldType: string } | null>(null)
const droppingField = ref(false)

// Edit field properties modal state
const editingField = ref<{ colName: string; field: any } | null>(null)
const editProps = reactive({ facet: false, sort: false, index: true, optional: false })
const applyingProps = ref(false)

// Add field modal state
const addingFieldTo = ref<string | null>(null)
const newFieldName = ref('')
const newFieldType = ref('string')
const newFieldFacet = ref(false)
const newFieldSort = ref(false)
const newFieldIndex = ref(true)
const newFieldOptional = ref(false)
const addingField = ref(false)

onMounted(() => schema.fetchCollections())

const refreshing = ref(false)
async function handleRefresh() {
  refreshing.value = true
  const done = schema.fetchCollections()
  const min = new Promise(r => setTimeout(r, 500))
  await Promise.all([done, min])
  refreshing.value = false
}

function copyFilter(field: any) {
  const expr = `${field.name}:=`
  navigator.clipboard.writeText(expr).then(() => toast.success('Filter expression copied'))
}

// --- Delete collection ---
function openDeleteCol(name: string) {
  deletingColTarget.value = name
  deleteConfirmInput.value = ''
}

function closeDeleteCol() {
  deletingColTarget.value = null
  deleteConfirmInput.value = ''
}

const deleteConfirmMatch = computed(() =>
  deletingColTarget.value !== null && deleteConfirmInput.value === deletingColTarget.value
)

async function confirmDeleteCol() {
  const name = deletingColTarget.value
  if (!name || !deleteConfirmMatch.value) return
  deletingCol.value = true
  try {
    await api.deleteCollection(name)
    toast.success(`Collection "${name}" deleted`)
    deletingColTarget.value = null
    schema.fetchCollections()
  } catch (e: any) {
    toast.error(e.message || 'Failed to delete collection')
  } finally {
    deletingCol.value = false
  }
}

// --- Drop field ---
function openDropField(colName: string, fieldName: string, fieldType: string) {
  droppingFieldTarget.value = { colName, fieldName, fieldType }
}

function closeDropField() {
  droppingFieldTarget.value = null
}

async function confirmDropField() {
  const t = droppingFieldTarget.value
  if (!t) return
  droppingField.value = true
  try {
    await api.updateCollection(t.colName, {
      fields: [{ name: t.fieldName, type: t.fieldType, drop: true }],
    })
    toast.success(`Field "${t.fieldName}" dropped`)
    droppingFieldTarget.value = null
    schema.fetchCollections()
  } catch (e: any) {
    toast.error(e.message || 'Failed to drop field')
  } finally {
    droppingField.value = false
  }
}

// --- Add field ---
function startAddField(colName: string) {
  addingFieldTo.value = colName
  newFieldName.value = ''
  newFieldType.value = 'string'
  newFieldFacet.value = false
  newFieldSort.value = false
  newFieldIndex.value = true
  newFieldOptional.value = false
}

function closeAddField() {
  addingFieldTo.value = null
}

const canAddField = computed(() => addingFieldTo.value && newFieldName.value.trim())

async function addField() {
  if (!canAddField.value) return
  addingField.value = true
  try {
    await api.updateCollection(addingFieldTo.value!, {
      fields: [{
        name: newFieldName.value.trim(),
        type: newFieldType.value,
        facet: newFieldFacet.value,
        sort: newFieldSort.value,
        index: newFieldIndex.value,
        optional: newFieldOptional.value,
      }],
    })
    toast.success(`Field "${newFieldName.value.trim()}" added`)
    addingFieldTo.value = null
    schema.fetchCollections()
  } catch (e: any) {
    toast.error(e.message || 'Failed to add field')
  } finally {
    addingField.value = false
  }
}

// --- Edit field properties ---
const SORTABLE_TYPES = new Set(['string', 'int32', 'int64', 'float'])
const INDEXABLE_TYPES = new Set(['string', 'string[]'])

function canToggle(field: any, prop: string): boolean {
  if (prop === 'sort') return SORTABLE_TYPES.has(field.type)
  if (prop === 'index') return INDEXABLE_TYPES.has(field.type)
  return true // facet, optional — all types
}

function openFieldProps(colName: string, field: any) {
  editingField.value = { colName, field }
  editProps.facet = !!field.facet
  editProps.sort = !!field.sort
  editProps.index = field.index !== false
  editProps.optional = !!field.optional
}

function closeFieldProps() {
  editingField.value = null
}

const propsChanged = computed(() => {
  const f = editingField.value?.field
  if (!f) return false
  return editProps.facet !== !!f.facet
    || editProps.sort !== !!f.sort
    || editProps.index !== (f.index !== false)
    || editProps.optional !== !!f.optional
})

async function applyFieldProps() {
  const e = editingField.value
  if (!e || !propsChanged.value) return
  applyingProps.value = true
  try {
    const patch: any = { name: e.field.name, type: e.field.type }
    const f = e.field
    if (editProps.facet !== !!f.facet) patch.facet = editProps.facet
    if (editProps.sort !== !!f.sort) patch.sort = editProps.sort
    if (editProps.index !== (f.index !== false)) patch.index = editProps.index
    if (editProps.optional !== !!f.optional) patch.optional = editProps.optional
    await api.updateCollection(e.colName, { fields: [patch] })
    const changed = Object.keys(patch).filter(k => k !== 'name' && k !== 'type')
    toast.success(`Updated "${e.field.name}" — changed ${changed.join(', ')}`)
    editingField.value = null
    schema.fetchCollections()
  } catch (err: any) {
    toast.error(err.message || 'Failed to update field properties')
  } finally {
    applyingProps.value = false
  }
}

const PROP_INFO: Record<string, { label: string; desc: string }> = {
  facet: { label: 'Facet', desc: 'Return value counts in search results for filtering UIs.' },
  sort: { label: 'Sort', desc: 'Allow sorting search results by this field.' },
  index: { label: 'Index', desc: 'Full-text index this field, making it searchable.' },
  optional: { label: 'Optional', desc: 'Accept documents that are missing this field.' },
}

// --- Raw schema viewer ---
const showRawSchema = ref(false)
const rawSchema = ref<string>('')
const loadingRaw = ref(false)

// --- Overlay stats for realtime collections ---
const overlayStats = ref<{ num_documents: number; num_tombstones: number; last_compaction_at: string | null } | null>(null)
const compacting = ref(false)

const isRealtime = computed(() => currentCol.value?.index_mode === 'realtime')

watch(() => conn.currentCollection, async (name) => {
  overlayStats.value = null
  showRawSchema.value = false
  if (!name) return
  try {
    const col = await api.getCollection(name)
    if (col.overlay) overlayStats.value = col.overlay
  } catch {}
}, { immediate: true })

async function compactNow() {
  if (!conn.currentCollection || compacting.value) return
  compacting.value = true
  try {
    await api.compactCollection(conn.currentCollection)
    toast.success('Compaction complete')
    const col = await api.getCollection(conn.currentCollection)
    if (col.overlay) overlayStats.value = col.overlay
  } catch (e: any) {
    toast.error(e.message || 'Compaction failed')
  } finally {
    compacting.value = false
  }
}

function formatRelativeTime(iso: string | null): string {
  if (!iso) return 'never'
  const diff = Date.now() - new Date(iso).getTime()
  if (diff < 60000) return 'just now'
  if (diff < 3600000) return `${Math.floor(diff / 60000)}m ago`
  if (diff < 86400000) return `${Math.floor(diff / 3600000)}h ago`
  return `${Math.floor(diff / 86400000)}d ago`
}

async function toggleRawSchema() {
  if (showRawSchema.value) {
    showRawSchema.value = false
    return
  }
  if (!conn.currentCollection) return
  loadingRaw.value = true
  try {
    const col = await api.getCollection(conn.currentCollection)
    rawSchema.value = JSON.stringify(col, null, 2)
    showRawSchema.value = true
  } catch (e: any) {
    toast.error(e.message || 'Failed to load schema')
  } finally {
    loadingRaw.value = false
  }
}

function copyRawSchema() {
  navigator.clipboard.writeText(rawSchema.value).then(() => toast.success('Schema JSON copied'))
}

function onCollectionCreated() {
  schema.fetchCollections()
}
</script>

<template>
  <div class="sb-root">
    <div class="sb-toolbar">
      <span class="sb-toolbar-title">Schema</span>
      <button
        class="fg-icon-btn"
        :disabled="refreshing"
        title="Refresh schema"
        @click="handleRefresh()"
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" :class="{ 'sb-spinning': refreshing }">
          <path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" /><path d="M21 3v5h-5" /><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" /><path d="M8 16H3v5" />
        </svg>
      </button>
    </div>

    <div class="sb-content">
      <!-- No collection selected -->
      <div v-if="!conn.currentCollection" class="sb-empty">
        Select a collection to view its schema
      </div>

      <!-- Selected collection not found -->
      <div v-else-if="!currentCol" class="sb-empty">
        Collection not found
      </div>

      <!-- Fields section -->
      <div v-else class="sb-section">
        <div class="sb-section-header">
          Fields
          <span class="sb-section-badge">{{ currentFields.length }}</span>
          <span v-if="isRealtime" class="sb-mode-badge sb-mode-badge--rt">REALTIME</span>
          <span v-else class="sb-mode-badge">BATCH</span>
        </div>
        <div class="sb-fields">
          <template v-for="node in fieldTree" :key="node.fullName">
            <!-- Regular field (no children) -->
            <div v-if="!node.children.length" class="sb-field">
              <div class="sb-field-main" title="Click to copy filter expression" @click="copyFilter(node.field)">
                <span class="sb-field-name">{{ node.name }}</span>
                <span class="sb-field-type">{{ node.field.type }}</span>
              </div>
              <div class="sb-field-right">
                <div class="sb-field-flags" @click.stop="openFieldProps(conn.currentCollection, node.field)" title="Click to edit field properties">
                  <span class="sb-flag" :class="{ 'sb-flag--on': node.field.facet }">facet</span>
                  <span v-if="canToggle(node.field, 'sort')" class="sb-flag" :class="{ 'sb-flag--on': node.field.sort }">sort</span>
                  <span v-if="canToggle(node.field, 'index')" class="sb-flag" :class="{ 'sb-flag--on': node.field.index !== false }">index</span>
                  <span class="sb-flag" :class="{ 'sb-flag--on': node.field.optional }">optional</span>
                </div>
                <button
                  class="sb-field-drop"
                  :disabled="currentFields.length <= 1"
                  @click.stop="openDropField(conn.currentCollection, node.field.name, node.field.type)"
                  :title="currentFields.length <= 1 ? 'Cannot drop the last field' : 'Drop field'"
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <path d="M18 6 6 18" /><path d="m6 6 12 12" />
                  </svg>
                </button>
              </div>
            </div>

            <!-- Nested group -->
            <div v-else class="sb-group">
              <div class="sb-group-header" @click="expandedGroups.has(node.name) ? expandedGroups.delete(node.name) : expandedGroups.add(node.name)">
                <svg
                  class="sb-chevron"
                  :class="{ 'sb-chevron--open': expandedGroups.has(node.name) }"
                  width="12" height="12" viewBox="0 0 24 24"
                  fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
                >
                  <path d="m9 18 6-6-6-6" />
                </svg>
                <span class="sb-group-name">{{ node.name }}</span>
                <span class="sb-group-type">object</span>
                <span class="sb-section-badge">{{ node.children.length }}</span>
              </div>
              <div v-if="expandedGroups.has(node.name)" class="sb-group-children">
                <div
                  v-for="child in node.children"
                  :key="child.fullName"
                  class="sb-field sb-field--nested"
                >
                  <div class="sb-field-main" title="Click to copy filter expression" @click="copyFilter(child.field)">
                    <span class="sb-field-name">{{ child.name }}</span>
                    <span class="sb-field-type">{{ child.field.type }}</span>
                  </div>
                  <div class="sb-field-right">
                    <div class="sb-field-flags" @click.stop="openFieldProps(conn.currentCollection, child.field)" title="Click to edit field properties">
                      <span class="sb-flag" :class="{ 'sb-flag--on': child.field.facet }">facet</span>
                      <span v-if="canToggle(child.field, 'sort')" class="sb-flag" :class="{ 'sb-flag--on': child.field.sort }">sort</span>
                      <span v-if="canToggle(child.field, 'index')" class="sb-flag" :class="{ 'sb-flag--on': child.field.index !== false }">index</span>
                      <span class="sb-flag" :class="{ 'sb-flag--on': child.field.optional }">optional</span>
                    </div>
                    <button
                      class="sb-field-drop"
                      :disabled="currentFields.length <= 1"
                      @click.stop="openDropField(conn.currentCollection, child.field.name, child.field.type)"
                      :title="currentFields.length <= 1 ? 'Cannot drop the last field' : 'Drop field'"
                    >
                      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                        <path d="M18 6 6 18" /><path d="m6 6 12 12" />
                      </svg>
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </template>

          <div v-if="!currentFields.length" class="sb-field-empty">No fields defined</div>
          <button
            class="sb-add-field-btn"
            @click.stop="startAddField(conn.currentCollection)"
          >
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M5 12h14" /><path d="M12 5v14" />
            </svg>
            Add field
          </button>
        </div>
      </div>
    </div>

    <!-- Overlay stats (realtime only) -->
    <div v-if="isRealtime && overlayStats" class="sb-overlay">
      <div class="sb-overlay-header">Overlay</div>
      <div class="sb-overlay-row">
        <span>Documents</span>
        <span class="mono">{{ overlayStats.num_documents }}</span>
      </div>
      <div class="sb-overlay-row">
        <span>Tombstones</span>
        <span class="mono">{{ overlayStats.num_tombstones }}</span>
      </div>
      <div class="sb-overlay-row">
        <span>Last compaction</span>
        <span class="mono">{{ formatRelativeTime(overlayStats.last_compaction_at) }}</span>
      </div>
      <button
        class="sb-compact-btn"
        :disabled="compacting"
        @click="compactNow"
      >
        {{ compacting ? 'Compacting...' : 'Compact Now' }}
      </button>
    </div>

    <!-- Raw schema viewer -->
    <div v-if="showRawSchema && currentCol" class="sb-raw">
      <div class="sb-raw-header">
        <span class="sb-raw-title">Raw Schema</span>
        <button class="fg-icon-btn" title="Copy JSON" @click="copyRawSchema">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <rect width="14" height="14" x="8" y="8" rx="2" /><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
          </svg>
        </button>
      </div>
      <pre class="sb-raw-json">{{ rawSchema }}</pre>
    </div>

    <!-- Collection actions -->
    <div v-if="currentCol" class="sb-actions">
      <button
        class="sb-action-btn"
        @click="showCreate = true"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M5 12h14" /><path d="M12 5v14" />
        </svg>
        New
      </button>
      <button
        class="sb-action-btn"
        :disabled="loadingRaw"
        @click="toggleRawSchema"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="m18 16 4-4-4-4" /><path d="m6 8-4 4 4 4" /><path d="m14.5 4-5 16" />
        </svg>
        {{ showRawSchema ? 'Hide JSON' : 'Raw JSON' }}
      </button>
      <button
        class="sb-action-btn sb-action-btn--danger"
        :title="conn.allowDropCollection ? 'Delete this collection' : 'Collection deletion is disabled on this server'"
        @click="conn.allowDropCollection ? openDeleteCol(conn.currentCollection) : (showDropDisabledModal = true)"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M3 6h18" /><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /><path d="M10 11v6" /><path d="M14 11v6" />
        </svg>
        Delete
      </button>
    </div>
    <div v-else class="sb-actions">
      <button
        class="sb-action-btn"
        @click="showCreate = true"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M5 12h14" /><path d="M12 5v14" />
        </svg>
        New Collection
      </button>
    </div>

    <CreateCollectionModal
      v-if="showCreate"
      @close="showCreate = false"
      @created="onCollectionCreated"
    />
  </div>

  <!-- Edit field properties modal -->
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="editingField" class="fg-modal-overlay" @mousedown.self="closeFieldProps">
        <div class="sb-props-panel" role="dialog" aria-modal="true" aria-label="Edit field properties">
          <div class="sb-props-header">
            <div>
              <span class="sb-props-title">Edit Field Properties</span>
              <span class="sb-props-subtitle">{{ editingField.field.name }} <span class="sb-props-type">{{ editingField.field.type }}</span></span>
            </div>
            <button class="sb-props-close" aria-label="Close" @click="closeFieldProps">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 6 6 18" /><path d="m6 6 12 12" />
              </svg>
            </button>
          </div>
          <div class="sb-props-body">
            <div class="sb-props-list">
              <label class="sb-prop-row">
                <div class="sb-prop-info">
                  <span class="sb-prop-label">{{ PROP_INFO.facet.label }}</span>
                  <span class="sb-prop-desc">{{ PROP_INFO.facet.desc }}</span>
                </div>
                <button
                  class="sb-prop-switch"
                  :class="{ 'sb-prop-switch--on': editProps.facet }"
                  @click="editProps.facet = !editProps.facet"
                  role="switch"
                  :aria-checked="editProps.facet"
                ><span class="sb-prop-switch-thumb" /></button>
              </label>
              <label v-if="canToggle(editingField.field, 'sort')" class="sb-prop-row">
                <div class="sb-prop-info">
                  <span class="sb-prop-label">{{ PROP_INFO.sort.label }}</span>
                  <span class="sb-prop-desc">{{ PROP_INFO.sort.desc }}</span>
                </div>
                <button
                  class="sb-prop-switch"
                  :class="{ 'sb-prop-switch--on': editProps.sort }"
                  @click="editProps.sort = !editProps.sort"
                  role="switch"
                  :aria-checked="editProps.sort"
                ><span class="sb-prop-switch-thumb" /></button>
              </label>
              <label v-if="canToggle(editingField.field, 'index')" class="sb-prop-row">
                <div class="sb-prop-info">
                  <span class="sb-prop-label">{{ PROP_INFO.index.label }}</span>
                  <span class="sb-prop-desc">{{ PROP_INFO.index.desc }}</span>
                </div>
                <button
                  class="sb-prop-switch"
                  :class="{ 'sb-prop-switch--on': editProps.index }"
                  @click="editProps.index = !editProps.index"
                  role="switch"
                  :aria-checked="editProps.index"
                ><span class="sb-prop-switch-thumb" /></button>
              </label>
              <label class="sb-prop-row">
                <div class="sb-prop-info">
                  <span class="sb-prop-label">{{ PROP_INFO.optional.label }}</span>
                  <span class="sb-prop-desc">{{ PROP_INFO.optional.desc }}</span>
                </div>
                <button
                  class="sb-prop-switch"
                  :class="{ 'sb-prop-switch--on': editProps.optional }"
                  @click="editProps.optional = !editProps.optional"
                  role="switch"
                  :aria-checked="editProps.optional"
                ><span class="sb-prop-switch-thumb" /></button>
              </label>
            </div>
            <div v-if="propsChanged" class="sb-props-warning">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" />
                <path d="M12 9v4" /><path d="M12 17h.01" />
              </svg>
              <span>This will trigger a full index rebuild on <strong>{{ editingField.colName }}</strong>. All documents are preserved, but searches may be briefly affected during the rebuild.</span>
            </div>
          </div>
          <div class="sb-props-footer">
            <button class="fg-btn" @click="closeFieldProps">Cancel</button>
            <button class="fg-btn fg-btn--primary" :disabled="applyingProps || !propsChanged" @click="applyFieldProps">
              {{ applyingProps ? 'Applying...' : 'Apply Changes' }}
            </button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>

  <!-- Drop field confirmation modal -->
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="droppingFieldTarget" class="fg-modal-overlay" @mousedown.self="closeDropField">
        <div class="sb-props-panel" role="dialog" aria-modal="true" aria-label="Drop field">
          <div class="sb-props-header">
            <div>
              <span class="sb-props-title">Drop Field</span>
              <span class="sb-props-subtitle">{{ droppingFieldTarget.fieldName }} from {{ droppingFieldTarget.colName }}</span>
            </div>
            <button class="sb-props-close" aria-label="Close" @click="closeDropField">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 6 6 18" /><path d="m6 6 12 12" />
              </svg>
            </button>
          </div>
          <div class="sb-props-body">
            <div class="sb-props-warning sb-props-warning--error">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" />
                <path d="M12 9v4" /><path d="M12 17h.01" />
              </svg>
              <span>This will permanently remove the field <strong>{{ droppingFieldTarget.fieldName }}</strong> and all its indexed data. The collection will be rebuilt without this field. This cannot be undone.</span>
            </div>
          </div>
          <div class="sb-props-footer">
            <button class="fg-btn" @click="closeDropField">Cancel</button>
            <button class="fg-btn fg-btn--danger" :disabled="droppingField" @click="confirmDropField">
              {{ droppingField ? 'Dropping...' : 'Drop Field' }}
            </button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>

  <!-- Add field modal -->
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="addingFieldTo" class="fg-modal-overlay" @mousedown.self="closeAddField">
        <div class="sb-props-panel" role="dialog" aria-modal="true" aria-label="Add field">
          <div class="sb-props-header">
            <div>
              <span class="sb-props-title">Add Field</span>
              <span class="sb-props-subtitle">to {{ addingFieldTo }}</span>
            </div>
            <button class="sb-props-close" aria-label="Close" @click="closeAddField">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 6 6 18" /><path d="m6 6 12 12" />
              </svg>
            </button>
          </div>
          <div class="sb-props-body">
            <div class="sb-add-row">
              <label class="sb-add-label">Name</label>
              <input
                v-model="newFieldName"
                class="fg-input sb-add-input"
                placeholder="field_name"
                @keydown.enter="addField"
              />
            </div>
            <div class="sb-add-row">
              <label class="sb-add-label">Type</label>
              <select v-model="newFieldType" class="fg-input sb-add-input sb-add-select">
                <option v-for="t in FIELD_TYPES" :key="t" :value="t">{{ t }}</option>
              </select>
            </div>
            <div class="sb-props-list sb-add-props">
              <label class="sb-prop-row">
                <div class="sb-prop-info">
                  <span class="sb-prop-label">{{ PROP_INFO.facet.label }}</span>
                  <span class="sb-prop-desc">{{ PROP_INFO.facet.desc }}</span>
                </div>
                <button class="sb-prop-switch" :class="{ 'sb-prop-switch--on': newFieldFacet }" @click="newFieldFacet = !newFieldFacet" role="switch" :aria-checked="newFieldFacet"><span class="sb-prop-switch-thumb" /></button>
              </label>
              <label v-if="SORTABLE_TYPES.has(newFieldType)" class="sb-prop-row">
                <div class="sb-prop-info">
                  <span class="sb-prop-label">{{ PROP_INFO.sort.label }}</span>
                  <span class="sb-prop-desc">{{ PROP_INFO.sort.desc }}</span>
                </div>
                <button class="sb-prop-switch" :class="{ 'sb-prop-switch--on': newFieldSort }" @click="newFieldSort = !newFieldSort" role="switch" :aria-checked="newFieldSort"><span class="sb-prop-switch-thumb" /></button>
              </label>
              <label v-if="INDEXABLE_TYPES.has(newFieldType)" class="sb-prop-row">
                <div class="sb-prop-info">
                  <span class="sb-prop-label">{{ PROP_INFO.index.label }}</span>
                  <span class="sb-prop-desc">{{ PROP_INFO.index.desc }}</span>
                </div>
                <button class="sb-prop-switch" :class="{ 'sb-prop-switch--on': newFieldIndex }" @click="newFieldIndex = !newFieldIndex" role="switch" :aria-checked="newFieldIndex"><span class="sb-prop-switch-thumb" /></button>
              </label>
              <label class="sb-prop-row">
                <div class="sb-prop-info">
                  <span class="sb-prop-label">{{ PROP_INFO.optional.label }}</span>
                  <span class="sb-prop-desc">{{ PROP_INFO.optional.desc }}</span>
                </div>
                <button class="sb-prop-switch" :class="{ 'sb-prop-switch--on': newFieldOptional }" @click="newFieldOptional = !newFieldOptional" role="switch" :aria-checked="newFieldOptional"><span class="sb-prop-switch-thumb" /></button>
              </label>
            </div>
          </div>
          <div class="sb-props-footer">
            <button class="fg-btn" @click="closeAddField">Cancel</button>
            <button class="fg-btn fg-btn--primary" :disabled="addingField || !canAddField" @click="addField">
              {{ addingField ? 'Adding...' : 'Add Field' }}
            </button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>

  <!-- Delete collection confirmation modal -->
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="deletingColTarget" class="fg-modal-overlay" @mousedown.self="closeDeleteCol">
        <div class="sb-props-panel" role="dialog" aria-modal="true" aria-label="Delete collection">
          <div class="sb-props-header">
            <div>
              <span class="sb-props-title">Delete Collection</span>
              <span class="sb-props-subtitle">{{ deletingColTarget }}</span>
            </div>
            <button class="sb-props-close" aria-label="Close" @click="closeDeleteCol">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 6 6 18" /><path d="m6 6 12 12" />
              </svg>
            </button>
          </div>
          <div class="sb-props-body">
            <div class="sb-props-warning sb-props-warning--error">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z" />
                <path d="M12 9v4" /><path d="M12 17h.01" />
              </svg>
              <span>This will permanently delete the collection <strong>{{ deletingColTarget }}</strong>, including all documents, fields, and indexed data. This cannot be undone.</span>
            </div>
            <div class="sb-delete-confirm">
              <label class="sb-add-label">Type <code>{{ deletingColTarget }}</code> to confirm</label>
              <input
                v-model="deleteConfirmInput"
                class="fg-input sb-add-input"
                :placeholder="deletingColTarget"
                @keydown.enter="deleteConfirmMatch && confirmDeleteCol()"
              />
            </div>
          </div>
          <div class="sb-props-footer">
            <button class="fg-btn" @click="closeDeleteCol">Cancel</button>
            <button class="fg-btn fg-btn--danger" :disabled="deletingCol || !deleteConfirmMatch" @click="confirmDeleteCol">
              {{ deletingCol ? 'Deleting...' : 'Delete Collection' }}
            </button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>

  <!-- Drop disabled info modal -->
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="showDropDisabledModal" class="fg-modal-overlay" @mousedown.self="showDropDisabledModal = false">
        <div class="sb-props-panel" role="dialog" aria-modal="true" aria-label="Deletion disabled">
          <div class="sb-props-header">
            <div>
              <span class="sb-props-title">Collection deletion disabled</span>
              <span class="sb-props-subtitle">This server doesn't permit drops</span>
            </div>
            <button class="sb-props-close" aria-label="Close" @click="showDropDisabledModal = false">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 6 6 18" /><path d="m6 6 12 12" />
              </svg>
            </button>
          </div>
          <div class="sb-props-body">
            <p>Collection deletion is disabled on this Torque server. The server administrator has turned it off to prevent accidental destructive operations.</p>
            <p>To enable it, set one of the following on the server:</p>
            <ul>
              <li><code>allow_drop_collection = true</code> in <code>torque.toml</code></li>
              <li><code>TORQUE_ALLOW_DROP_COLLECTION=true</code> environment variable</li>
              <li><code>--allow-drop-collection</code> command-line flag</li>
            </ul>
            <p>Then restart the server.</p>
          </div>
          <div class="sb-props-footer">
            <button class="fg-btn" @click="showDropDisabledModal = false">Close</button>
          </div>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.sb-root {
  display: flex;
  flex-direction: column;
  height: 100%;
}

.sb-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 12px;
  flex-shrink: 0;
}

.sb-toolbar-title {
  font-size: var(--tv-font-size-sm);
  font-weight: 600;
  color: var(--tv-text-secondary);
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.sb-content {
  flex: 1;
  overflow-y: auto;
  min-height: 0;
}

.sb-empty {
  color: var(--tv-text-muted);
  font-size: var(--tv-font-size-sm);
  padding: 20px 12px;
}

/* Section header */
.sb-section-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  font-size: var(--tv-font-size-sm);
  font-weight: 500;
  color: var(--tv-text-primary);
}

.sb-section-badge {
  font-size: var(--tv-font-size-xs);
  font-weight: 500;
  color: var(--tv-text-secondary);
  background: var(--tv-bg-inset);
  border-radius: 9px;
  padding: 1px 7px;
  line-height: 18px;
  font-family: var(--tv-font-mono);
}

/* Index mode badge */
.sb-mode-badge {
  font-size: 10px;
  font-weight: 600;
  padding: 1px 6px;
  border-radius: 3px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--tv-text-muted);
  border: 1px solid var(--tv-border-default);
  margin-left: auto;
}
.sb-mode-badge--rt {
  background: var(--tv-accent);
  border-color: var(--tv-accent);
  color: var(--tv-text-inverse);
}

/* Overlay stats */
.sb-overlay {
  padding: 8px 12px;
  border-top: 1px solid var(--tv-border-default);
  flex-shrink: 0;
}

.sb-overlay-header {
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 6px;
}

.sb-overlay-row {
  display: flex;
  justify-content: space-between;
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-secondary);
  padding: 2px 0;
}

.sb-compact-btn {
  display: block;
  width: 100%;
  margin-top: 8px;
  padding: 5px 8px;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-sm);
  background: none;
  color: var(--tv-text-secondary);
  font-size: var(--tv-font-size-xs);
  font-family: inherit;
  cursor: pointer;
  transition: background 0.15s, color 0.15s, border-color 0.15s;
}
.sb-compact-btn:hover:not(:disabled) {
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}
.sb-compact-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

/* Raw schema viewer */
.sb-raw {
  border-top: 1px solid var(--tv-border-default);
  flex-shrink: 0;
  max-height: 300px;
  display: flex;
  flex-direction: column;
}

.sb-raw-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 6px 12px;
  flex-shrink: 0;
}

.sb-raw-title {
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.sb-raw-json {
  margin: 0;
  padding: 0 12px 8px;
  font-size: 11px;
  font-family: var(--tv-font-mono);
  color: var(--tv-text-secondary);
  line-height: 1.5;
  overflow: auto;
  white-space: pre;
  flex: 1;
  min-height: 0;
}

/* Field list */
.sb-fields {
  padding: 0 8px 8px;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.sb-field {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 7px 10px;
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  transition: border-color 0.15s;
}
.sb-field:hover {
  border-color: var(--tv-border-strong);
}

.sb-field-main {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  flex: 1;
  cursor: pointer;
  border-radius: 3px;
  padding: 2px 4px;
  margin: -2px -4px;
  transition: background 0.1s;
}
.sb-field-main:hover {
  background: var(--tv-bg-hover);
}

.sb-field-name {
  font-size: var(--tv-font-size-xs);
  font-weight: 500;
  color: var(--tv-text-primary);
  font-family: var(--tv-font-mono);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.sb-field-type {
  font-size: 11px;
  color: var(--tv-accent);
  font-family: var(--tv-font-mono);
  flex-shrink: 0;
}

.sb-field-right {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
}

.sb-field-flags {
  display: flex;
  gap: 4px;
  cursor: pointer;
  border-radius: 3px;
  padding: 1px 2px;
  margin: -1px -2px;
  transition: background 0.1s;
}
.sb-field-flags:hover {
  background: var(--tv-bg-hover);
}

.sb-flag {
  font-size: 10px;
  font-weight: 500;
  padding: 1px 5px;
  border-radius: 3px;
  text-transform: uppercase;
  letter-spacing: 0.03em;
  color: var(--tv-text-muted);
  border: 1px solid var(--tv-border-default);
  background: none;
}
.sb-flag--on {
  background: var(--tv-accent);
  border-color: var(--tv-accent);
  color: var(--tv-text-inverse);
}

.sb-field-drop {
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
.sb-field:hover .sb-field-drop {
  opacity: 1;
}
.sb-field-drop:hover:not(:disabled) {
  color: var(--tv-status-error);
  background: var(--tv-bg-hover);
}
.sb-field-drop:disabled {
  opacity: 0.3;
  cursor: not-allowed;
}

/* Nested group */
.sb-group {
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  overflow: hidden;
  transition: border-color 0.15s;
}
.sb-group:hover {
  border-color: var(--tv-border-strong);
}

.sb-group-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 7px 10px;
  cursor: pointer;
  transition: background 0.1s;
}
.sb-group-header:hover {
  background: var(--tv-bg-hover);
}

.sb-chevron {
  flex-shrink: 0;
  color: var(--tv-text-muted);
  transition: transform 0.15s;
}
.sb-chevron--open {
  transform: rotate(90deg);
}

.sb-group-name {
  font-size: var(--tv-font-size-xs);
  font-weight: 500;
  color: var(--tv-text-primary);
  font-family: var(--tv-font-mono);
}

.sb-group-type {
  font-size: 11px;
  color: var(--tv-text-muted);
  font-family: var(--tv-font-mono);
  font-style: italic;
}

.sb-group-children {
  border-top: 1px solid var(--tv-border-default);
  padding: 4px 0;
}

.sb-field--nested {
  background: none;
  border: none;
  border-radius: 0;
  padding: 5px 10px 5px 28px;
}
.sb-field--nested:hover {
  border-color: transparent;
}
.sb-field--nested + .sb-field--nested {
  border-top: 1px solid var(--tv-border-subtle);
}
.sb-field--nested .sb-field-name {
  color: var(--tv-text-secondary);
}

.sb-field-empty {
  padding: 8px 10px;
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-muted);
}

/* Add field button */
.sb-add-field-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  margin: 0;
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
.sb-add-field-btn:hover {
  color: var(--tv-text-primary);
  background: var(--tv-bg-hover);
}

/* Add field modal form rows */
.sb-add-row {
  display: flex;
  flex-direction: column;
  gap: 4px;
  margin-bottom: 12px;
}

.sb-add-label {
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.sb-add-label code {
  text-transform: none;
  font-family: var(--tv-font-mono);
  color: var(--tv-text-primary);
}

.sb-add-input {
  width: 100%;
  box-sizing: border-box;
}

.sb-add-select {
  font-family: var(--tv-font-mono);
  cursor: pointer;
  appearance: auto;
}

.sb-add-props {
  margin-top: 4px;
  border-top: 1px solid var(--tv-border-subtle);
  padding-top: 4px;
}

.sb-delete-confirm {
  margin-top: 16px;
}

/* Collection actions footer */
.sb-actions {
  flex-shrink: 0;
  border-top: 1px solid var(--tv-border-default);
  padding: 8px 12px;
  display: flex;
  gap: 6px;
}

.sb-action-btn {
  display: flex;
  align-items: center;
  gap: 5px;
  flex: 1;
  justify-content: center;
  padding: 5px 8px;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-sm);
  background: none;
  color: var(--tv-text-secondary);
  font-size: var(--tv-font-size-xs);
  font-family: var(--tv-font-sans);
  cursor: pointer;
  transition: background 0.15s, color 0.15s, border-color 0.15s;
}
.sb-action-btn:hover {
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}

.sb-action-btn--danger {
  border-color: var(--tv-status-error);
  color: var(--tv-status-error);
}
.sb-action-btn--danger:hover {
  background: var(--tv-status-error-subtle);
  color: var(--tv-status-error);
}

@keyframes sb-spin {
  to { transform: rotate(360deg); }
}
.sb-spinning {
  animation: sb-spin 0.6s linear infinite;
}

/* Edit field properties modal */
.sb-props-panel {
  width: 420px;
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-lg);
  box-shadow: var(--tv-shadow-lg);
}

.sb-props-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  padding: 16px 20px 12px;
  border-bottom: 1px solid var(--tv-border-default);
}

.sb-props-title {
  display: block;
  font-size: 14px;
  font-weight: 600;
  color: var(--tv-text-primary);
}

.sb-props-subtitle {
  display: block;
  font-size: var(--tv-font-size-xs);
  font-family: var(--tv-font-mono);
  color: var(--tv-text-secondary);
  margin-top: 2px;
}

.sb-props-type {
  color: var(--tv-accent);
}

.sb-props-close {
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
.sb-props-close:hover {
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}

.sb-props-body {
  padding: 16px 20px;
}

.sb-props-list {
  display: flex;
  flex-direction: column;
}

.sb-prop-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 0;
  cursor: pointer;
}
.sb-prop-row + .sb-prop-row {
  border-top: 1px solid var(--tv-border-subtle);
}

.sb-prop-info {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.sb-prop-label {
  font-size: var(--tv-font-size-sm);
  font-weight: 600;
  color: var(--tv-text-primary);
}

.sb-prop-desc {
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-muted);
  line-height: 1.4;
}

/* Toggle switch */
.sb-prop-switch {
  position: relative;
  flex-shrink: 0;
  width: 36px;
  height: 20px;
  border-radius: 10px;
  border: none;
  background: var(--tv-bg-inset);
  cursor: pointer;
  transition: background 0.2s;
  padding: 0;
}
.sb-prop-switch--on {
  background: var(--tv-accent);
}
.sb-prop-switch-thumb {
  position: absolute;
  top: 2px;
  left: 2px;
  width: 16px;
  height: 16px;
  border-radius: 50%;
  background: white;
  transition: transform 0.2s;
  box-shadow: 0 1px 3px rgba(0,0,0,0.2);
}
.sb-prop-switch--on .sb-prop-switch-thumb {
  transform: translateX(16px);
}

.sb-props-warning {
  display: flex;
  gap: 8px;
  padding: 10px 12px;
  margin-top: 16px;
  border-radius: var(--tv-radius-md);
  background: color-mix(in srgb, var(--tv-status-warning) 10%, transparent);
  border: 1px solid color-mix(in srgb, var(--tv-status-warning) 30%, transparent);
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-secondary);
  line-height: 1.5;
}
.sb-props-warning svg {
  flex-shrink: 0;
  color: var(--tv-status-warning);
  margin-top: 1px;
}

.sb-props-footer {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 20px;
  border-top: 1px solid var(--tv-border-default);
}

.sb-props-warning--error {
  background: color-mix(in srgb, var(--tv-status-error) 10%, transparent);
  border-color: color-mix(in srgb, var(--tv-status-error) 30%, transparent);
}
.sb-props-warning--error svg {
  color: var(--tv-status-error);
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

