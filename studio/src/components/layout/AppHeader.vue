<script setup lang="ts">
import { ref, computed, watch, onBeforeUnmount } from 'vue'
import { useConnectionStore } from '../../stores/connection'
import { useQueryStore } from '../../stores/query'
import { useTheme } from '../../composables/useTheme'
import * as api from '../../utils/api'
import AliasPanel from '../aliases/AliasPanel.vue'
import CreateCollectionModal from '../collections/CreateCollectionModal.vue'

const conn = useConnectionStore()
const query = useQueryStore()
const { theme, toggleTheme } = useTheme()

// Logout — only shown when auth is enabled and user has a stored key
const showLogout = computed(() => !!api.getApiKey() && !conn.authRequired)

function logout() {
  api.clearApiKey()
  conn.authRequired = true
}

// Alias panel
const showAliasPanel = ref(false)

// Create collection modal
const showCreateCol = ref(false)

// Collection dropdown
const showColMenu = ref(false)
const colError = ref('')

// Aliases: flat list fetched when dropdown opens
const aliasList = ref<{ name: string; collection_name: string }[]>([])

// Merged list: collections + aliases as top-level items
const colMenuItems = computed(() => {
  const items: { name: string; isAlias: boolean; target?: string; col?: any }[] = []
  for (const col of conn.collectionList) {
    items.push({ name: col.name, isAlias: false, col })
  }
  for (const { name, collection_name } of aliasList.value) {
    items.push({ name, isAlias: true, target: collection_name })
  }
  return items
})

function selectCollection(name: string) {
  conn.setCurrentCollection(name)
  showColMenu.value = false
}

function openCreateCollection() {
  showColMenu.value = false
  showCreateCol.value = true
}

function onClickOutside(e: MouseEvent) {
  const target = e.target as HTMLElement
  if (!target.closest('.header__db')) {
    showColMenu.value = false
  }
}

watch(showColMenu, async (open) => {
  if (open) {
    colError.value = ''
    document.addEventListener('click', onClickOutside)
    // Fetch aliases to show in the dropdown
    try {
      const resp = await api.aliases()
      aliasList.value = resp.aliases || []
    } catch {
      aliasList.value = []
    }
  } else {
    document.removeEventListener('click', onClickOutside)
  }
})

function formatBytes(bytes: number): string {
  const mb = bytes / (1024 * 1024)
  return mb < 1024 ? `${mb.toFixed(0)} MB` : `${(mb / 1024).toFixed(1)} GB`
}

const totalMemoryDisplay = computed(() => {
  if (!conn.memory?.resident_bytes) return ''
  return formatBytes(conn.memory.resident_bytes)
})

// Current collection stats from admin/stats
const currentColStats = computed(() => {
  if (!conn.currentCollection) return null
  return conn.collectionStats.find(c => c.name === conn.currentCollection) ?? null
})

const numDocsDisplay = computed(() => {
  // Prefer collectionStats (from admin/stats polling), fall back to collectionList
  if (currentColStats.value) return currentColStats.value.num_documents.toLocaleString()
  const col = conn.collectionList.find((c: any) => c.name === conn.currentCollection)
  if (col) return (col.num_documents ?? 0).toLocaleString()
  return ''
})

const colMemoryDisplay = computed(() => {
  if (!currentColStats.value?.memory?.total_bytes) return ''
  return formatBytes(currentColStats.value.memory.total_bytes)
})

const colMemoryPct = computed(() => {
  if (!currentColStats.value?.memory?.total_bytes || !conn.memory?.resident_bytes) return 0
  return Math.round((currentColStats.value.memory.total_bytes / conn.memory.resident_bytes) * 100)
})

const bytesPerDoc = computed(() => {
  const stats = currentColStats.value
  if (!stats?.memory?.total_bytes || !stats.num_documents) return ''
  const bpd = stats.memory.total_bytes / stats.num_documents
  return bpd < 1024 ? `${bpd.toFixed(0)} B` : `${(bpd / 1024).toFixed(1)} KB`
})

const lastQueryTime = computed(() => {
  // Prefer server-reported execution time (microseconds) over client round-trip
  const r = query.results
  if (r?.search_time_us != null) {
    const us = r.search_time_us
    if (us >= 1_000_000) return `${(us / 1_000_000).toFixed(2)}s`
    if (us >= 1_000) return `${(us / 1_000).toFixed(2)}ms`
    return `${us}\u00B5s`
  }
  if (r?.search_time_ms != null && r.search_time_ms > 0) return `${r.search_time_ms}ms`
  if (!query.timeMs) return ''
  return query.timeMs < 1 ? `${(query.timeMs * 1000).toFixed(0)}\u00B5s` : `${query.timeMs.toFixed(1)}ms`
})

// Memory popover
const memoryRef = ref<HTMLElement | null>(null)
const showMemoryTip = ref(false)
const memoryTipPos = ref({ top: 0, right: 0 })

function toggleMemoryPopover() {
  if (showMemoryTip.value) {
    showMemoryTip.value = false
    return
  }
  if (memoryRef.value) {
    const rect = memoryRef.value.getBoundingClientRect()
    memoryTipPos.value = {
      top: rect.bottom + 8,
      right: window.innerWidth - rect.right,
    }
  }
  showMemoryTip.value = true
}

function onClickOutsideMemory(e: MouseEvent) {
  const target = e.target as HTMLElement
  if (!target.closest('.memtip') && !target.closest('.header__memory')) {
    showMemoryTip.value = false
  }
}

watch(showMemoryTip, (open) => {
  if (open) {
    document.addEventListener('click', onClickOutsideMemory)
  } else {
    document.removeEventListener('click', onClickOutsideMemory)
  }
})

onBeforeUnmount(() => {
  document.removeEventListener('click', onClickOutsideMemory)
})
</script>

<template>
  <header class="header">
    <div class="header__left">
      <!-- Brand -->
      <span class="header__brand">Torque Studio</span>
      <span class="header__sep" />

      <!-- Collection selector -->
      <div v-if="conn.isConnected" class="header__db">
        <button class="header__db-btn" @click="showColMenu = !showColMenu">
          <span class="fg-dot fg-dot--green" />
          <span class="header__db-name">{{ conn.currentCollection || 'No collection' }}</span>
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="m6 9 6 6 6-6" />
          </svg>
        </button>

        <!-- Collection dropdown menu -->
        <div v-if="showColMenu" class="header__db-menu">
          <div class="header__db-menu-header">Collections</div>

          <button
            v-for="item in colMenuItems"
            :key="item.name"
            class="header__db-menu-item"
            :class="{ 'header__db-menu-item--active': item.name === conn.currentCollection }"
            @click="selectCollection(item.name)"
          >
            <span class="header__db-menu-name">
              {{ item.name }}
              <span v-if="item.isAlias" class="header__db-menu-badge header__db-menu-badge--alias">alias</span>
            </span>
            <span class="header__db-menu-info">
              <template v-if="item.isAlias">&rarr; {{ item.target }}</template>
              <template v-else>{{ (item.col.num_documents ?? 0).toLocaleString() }} docs</template>
            </span>
          </button>

          <div class="header__db-menu-divider" />

          <!-- Manage aliases -->
          <button class="header__db-menu-action" @click="showAliasPanel = true; showColMenu = false">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" />
              <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" />
            </svg>
            Manage aliases
          </button>

          <!-- Create new collection -->
          <button class="header__db-menu-action" @click="openCreateCollection">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M5 12h14" /><path d="M12 5v14" />
            </svg>
            New collection
          </button>

          <div v-if="colError" class="header__db-menu-error">{{ colError }}</div>
        </div>
      </div>
      <span v-else class="header__item header__disconnected">
        <span class="fg-dot fg-dot--red" />
        Disconnected
      </span>
    </div>

    <div class="header__right">
      <!-- Doc count + fields -->
      <span v-if="conn.currentCollection && numDocsDisplay" class="header__item header__item--muted">
        {{ numDocsDisplay }} docs
      </span>

      <!-- Memory: collection / total -->
      <span
        v-if="colMemoryDisplay || totalMemoryDisplay"
        ref="memoryRef"
        class="header__item header__item--muted header__memory"
        @click.stop="toggleMemoryPopover"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M6 19v2M10 19v2M14 19v2M18 19v2" /><rect x="4" y="6" width="16" height="13" rx="2" /><path d="M4 10h16" />
        </svg>
        <template v-if="colMemoryDisplay && totalMemoryDisplay">{{ colMemoryDisplay }} / {{ totalMemoryDisplay }}</template>
        <template v-else>{{ totalMemoryDisplay }}</template>
      </span>

      <Teleport to="body">
        <Transition name="memtip">
          <div
            v-if="showMemoryTip"
            class="memtip"
            :style="{ top: memoryTipPos.top + 'px', right: memoryTipPos.right + 'px' }"
            @click.stop
          >
            <div class="memtip__title">Memory</div>

            <div v-if="colMemoryDisplay" class="memtip__row">
              <span class="memtip__label">{{ conn.currentCollection }}</span>
              <span class="memtip__value">{{ colMemoryDisplay }}</span>
            </div>
            <div v-if="colMemoryDisplay && totalMemoryDisplay" class="memtip__bar">
              <div class="memtip__bar-fill" :style="{ width: colMemoryPct + '%' }" />
            </div>
            <div v-if="totalMemoryDisplay" class="memtip__row">
              <span class="memtip__label">Server total</span>
              <span class="memtip__value">{{ totalMemoryDisplay }}</span>
            </div>

            <div v-if="bytesPerDoc" class="memtip__divider" />
            <div v-if="bytesPerDoc" class="memtip__row">
              <span class="memtip__label">Per document</span>
              <span class="memtip__value">{{ bytesPerDoc }}</span>
            </div>
            <div v-if="bytesPerDoc" class="memtip__hint">
              {{ numDocsDisplay }} documents
            </div>
          </div>
        </Transition>
      </Teleport>

      <!-- Last query time -->
      <span v-if="lastQueryTime" class="header__item header__item--muted">
        {{ lastQueryTime }}
      </span>

      <!-- Version -->
      <span v-if="conn.version" class="header__version header__item header__item--muted">
        v{{ conn.version }}
      </span>

      <AliasPanel v-if="showAliasPanel" @close="showAliasPanel = false" />
      <CreateCollectionModal v-if="showCreateCol" @close="showCreateCol = false" />

      <!-- Theme toggle -->
      <button class="fg-icon-btn" :title="`Switch to ${theme === 'dark' ? 'light' : 'dark'} theme`" @click="toggleTheme">
        <svg v-if="theme === 'dark'" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <circle cx="12" cy="12" r="4" /><path d="M12 2v2" /><path d="M12 20v2" /><path d="m4.93 4.93 1.41 1.41" /><path d="m17.66 17.66 1.41 1.41" /><path d="M2 12h2" /><path d="M20 12h2" /><path d="m6.34 17.66-1.41 1.41" /><path d="m19.07 4.93-1.41 1.41" />
        </svg>
        <svg v-else width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
        </svg>
      </button>

      <!-- Logout -->
      <button v-if="showLogout" class="fg-icon-btn" title="Logout" @click="logout">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" /><polyline points="16 17 21 12 16 7" /><line x1="21" y1="12" x2="9" y2="12" />
        </svg>
      </button>
    </div>
  </header>
</template>

<style scoped>
.header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: var(--tv-header-height);
  width: 100%;
  padding: 0 12px;
  background: var(--tv-bg-surface);
  border-bottom: 1px solid var(--tv-border-default);
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-secondary);
  flex-shrink: 0;
}

.header__left,
.header__right {
  display: flex;
  align-items: center;
  gap: 10px;
}

.header__item {
  display: flex;
  align-items: center;
  gap: 4px;
  white-space: nowrap;
}

.header__brand {
  font-size: var(--tv-font-size-sm);
  font-weight: 600;
  color: var(--tv-text-secondary);
  white-space: nowrap;
  letter-spacing: -0.01em;
}

.header__disconnected {
  color: var(--tv-status-error);
}

.header__item--muted {
  color: var(--tv-text-muted);
  font-family: var(--tv-font-mono);
}

.header__memory {
  cursor: pointer;
  padding: 2px 6px;
  border-radius: var(--tv-radius-sm);
  transition: background 0.15s;
}
.header__memory:hover {
  background: var(--tv-bg-hover);
}

.header__sep {
  width: 1px;
  height: 16px;
  background: var(--tv-border-default);
}

/* Collection selector button */
.header__db {
  position: relative;
}

.header__db-btn {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 2px 6px;
  height: 26px;
  border: 1px solid transparent;
  border-radius: var(--tv-radius-sm);
  background: none;
  color: var(--tv-text-primary);
  font-size: var(--tv-font-size-xs);
  font-family: var(--tv-font-sans);
  cursor: pointer;
  transition: background 0.15s, border-color 0.15s;
}
.header__db-btn:hover {
  background: var(--tv-bg-hover);
  border-color: var(--tv-border-default);
}

.header__db-name {
  font-weight: 500;
  max-width: 120px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Collection dropdown menu */
.header__db-menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  z-index: 200;
  min-width: 260px;
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  box-shadow: var(--tv-shadow-lg);
  overflow: hidden;
}

.header__db-menu-header {
  padding: 8px 12px 4px;
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.header__db-menu-item {
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding: 6px 12px;
  cursor: pointer;
  transition: background 0.1s;
  width: 100%;
  border: none;
  background: none;
  text-align: left;
  font-family: inherit;
}
.header__db-menu-item:hover {
  background: var(--tv-bg-hover);
}
.header__db-menu-item--active {
  background: var(--tv-bg-hover);
  border-left: 2px solid var(--tv-accent);
}

.header__db-menu-name {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: var(--tv-font-size-sm);
  font-weight: 500;
  color: var(--tv-text-primary);
}

.header__db-menu-info {
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-muted);
  white-space: nowrap;
}

.header__db-menu-badge {
  font-size: 11px;
  font-weight: 500;
  padding: 1px 6px;
  border-radius: 3px;
  background: var(--tv-bg-elevated);
  color: var(--tv-text-muted);
  line-height: 1.5;
}

.header__db-menu-badge--alias {
  background: var(--tv-accent-subtle);
  color: var(--tv-accent);
}

.header__db-menu-divider {
  height: 1px;
  margin: 4px 0;
  background: var(--tv-border-default);
}

.header__db-menu-action {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 12px;
  width: 100%;
  border: none;
  background: none;
  font-family: inherit;
  font-size: var(--tv-font-size-sm);
  color: var(--tv-text-secondary);
  cursor: pointer;
  text-align: left;
  transition: background 0.1s;
}
.header__db-menu-action:hover {
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}

.header__db-menu-error {
  padding: 4px 12px 8px;
  font-size: var(--tv-font-size-xs);
  color: var(--tv-status-error);
}
</style>

<style>
/* Memory tooltip (teleported to body, cannot be scoped) */
.memtip {
  position: fixed;
  z-index: 10000;
  width: 220px;
  padding: 10px 12px;
  background: var(--tv-bg-elevated);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-lg);
  box-shadow: var(--tv-shadow-lg);
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-secondary);
}

.memtip__title {
  font-family: var(--tv-font-sans);
  font-size: var(--tv-font-size-xs);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin-bottom: 8px;
}

.memtip__row {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  padding: 2px 0;
}

.memtip__label {
  font-family: var(--tv-font-sans);
  color: var(--tv-text-secondary);
}

.memtip__value {
  font-family: var(--tv-font-mono);
  font-weight: 500;
  color: var(--tv-text-primary);
}

.memtip__bar {
  height: 4px;
  margin: 6px 0;
  background: var(--tv-bg-base);
  border-radius: 2px;
  overflow: hidden;
}

.memtip__bar-fill {
  height: 100%;
  min-width: 6px;
  background: var(--tv-accent);
  border-radius: 2px;
  transition: width 0.3s ease;
}

.memtip__divider {
  height: 1px;
  margin: 8px 0;
  background: var(--tv-border-default);
}

.memtip__hint {
  font-family: var(--tv-font-sans);
  font-size: 11px;
  color: var(--tv-text-muted);
  padding-top: 2px;
}

/* Transition */
.memtip-enter-active,
.memtip-leave-active {
  transition: opacity 0.15s ease;
}
.memtip-enter-from,
.memtip-leave-to {
  opacity: 0;
}

/* Version */
.header__version {
  display: flex;
  align-items: center;
  gap: 4px;
  white-space: nowrap;
  cursor: default;
}

</style>
