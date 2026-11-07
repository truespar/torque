<script setup lang="ts">
import { ref, computed, watch, onMounted, onUnmounted } from 'vue'
import { useConnectionStore } from '../../stores/connection'
import { useSettingsStore } from '../../stores/settings'
import { useQueryStore } from '../../stores/query'
import { useTheme } from '../../composables/useTheme'
import AppHeader from './AppHeader.vue'
import ActivityBar from './ActivityBar.vue'
import SchemaBrowser from '../schema/SchemaBrowser.vue'
import KeysBrowser from '../keys/KeysBrowser.vue'
import ImportPanel from '../import/ImportPanel.vue'
import ExamplesPanel from '../examples/ExamplesPanel.vue'
import SettingsPanel from '../settings/SettingsPanel.vue'
import SearchRefPanel from '../reference/SearchRefPanel.vue'
import DocsPanel from '../docs/DocsPanel.vue'
import QueryEditor from '../editor/QueryEditor.vue'
import ResultPanel from '../results/ResultPanel.vue'
import AppToast from '../ui/AppToast.vue'
import AuthGate from '../auth/AuthGate.vue'
import DashboardPanel from '../dashboard/DashboardPanel.vue'
import BenchmarkPanel from '../dashboard/BenchmarkPanel.vue'
import QuickSearchPanel from '../search/QuickSearchPanel.vue'
import AiChatPanel from '../chat/AiChatPanel.vue'

const conn = useConnectionStore()
const settings = useSettingsStore()
const queryStore = useQueryStore()
const { initTheme } = useTheme()

// Panels that render in the main content area instead of the sidebar
const MAIN_PANELS = new Set(['dashboard', 'quicksearch', 'benchmark', 'reference', 'docs'])
const sidebarPanel = computed(() =>
  settings.activePanel && !MAIN_PANELS.has(settings.activePanel) ? settings.activePanel : null
)
const mainPanel = computed(() =>
  settings.activePanel && MAIN_PANELS.has(settings.activePanel) ? settings.activePanel : null
)

function loadInt(key: string, fallback: number): number {
  const v = localStorage.getItem(key)
  if (!v) return fallback
  const n = parseInt(v, 10)
  return Number.isNaN(n) ? fallback : n
}

const sidePanelWidth = ref(loadInt('tq_sidebar_width', 260))
const editorHeight = ref(loadInt('tq_editor_height', 200))
const chatPanelWidth = ref(loadInt('tq_chat_panel_width', 380))
const isResizingSidePanel = ref(false)
const isResizingEditor = ref(false)
const isResizingChatPanel = ref(false)

watch(sidePanelWidth, (v) => localStorage.setItem('tq_sidebar_width', String(v)))
watch(editorHeight, (v) => localStorage.setItem('tq_editor_height', String(v)))
watch(chatPanelWidth, (v) => localStorage.setItem('tq_chat_panel_width', String(v)))

onMounted(() => {
  initTheme()
  conn.startPolling()
  if (conn.currentCollection) {
    queryStore.loadDraft(conn.currentCollection)
  }
  queryStore.fetchSavedQueries()
})

// Re-fetch when collection changes
watch(() => conn.currentCollection, () => {
  if (!conn.isConnected || conn.authRequired) return
})

onUnmounted(() => {
  conn.stopPolling()
  for (const cleanup of resizeCleanups) cleanup()
  resizeCleanups.length = 0
})

// Track active resize cleanup functions
const resizeCleanups: Array<() => void> = []

function startResizeSidePanel(e: MouseEvent) {
  isResizingSidePanel.value = true
  const startX = e.clientX
  const startW = sidePanelWidth.value

  const onMove = (ev: MouseEvent) => {
    const delta = ev.clientX - startX
    sidePanelWidth.value = Math.max(180, Math.min(500, startW + delta))
  }
  const onUp = () => {
    isResizingSidePanel.value = false
    document.removeEventListener('mousemove', onMove)
    document.removeEventListener('mouseup', onUp)
    resizeCleanups.splice(resizeCleanups.indexOf(onUp), 1)
  }
  document.addEventListener('mousemove', onMove)
  document.addEventListener('mouseup', onUp)
  resizeCleanups.push(onUp)
}

function startResizeChatPanel(e: MouseEvent) {
  isResizingChatPanel.value = true
  const startX = e.clientX
  const startW = chatPanelWidth.value

  const onMove = (ev: MouseEvent) => {
    const delta = startX - ev.clientX // inverted: dragging left makes panel wider
    chatPanelWidth.value = Math.max(280, Math.min(600, startW + delta))
  }
  const onUp = () => {
    isResizingChatPanel.value = false
    document.removeEventListener('mousemove', onMove)
    document.removeEventListener('mouseup', onUp)
    resizeCleanups.splice(resizeCleanups.indexOf(onUp), 1)
  }
  document.addEventListener('mousemove', onMove)
  document.addEventListener('mouseup', onUp)
  resizeCleanups.push(onUp)
}

function startResizeEditor(e: MouseEvent) {
  isResizingEditor.value = true
  const startY = e.clientY
  const startH = editorHeight.value

  const onMove = (ev: MouseEvent) => {
    const delta = ev.clientY - startY
    editorHeight.value = Math.max(100, Math.min(600, startH + delta))
  }
  const onUp = () => {
    isResizingEditor.value = false
    document.removeEventListener('mousemove', onMove)
    document.removeEventListener('mouseup', onUp)
    resizeCleanups.splice(resizeCleanups.indexOf(onUp), 1)
  }
  document.addEventListener('mousemove', onMove)
  document.addEventListener('mouseup', onUp)
  resizeCleanups.push(onUp)
}
</script>

<template>
  <!-- Login page: shown instead of app when auth is required -->
  <AuthGate v-if="conn.initialCheckDone && conn.authRequired" />

  <!-- Main app: only visible after auth -->
  <div v-show="conn.initialCheckDone && !conn.authRequired" class="app-shell">
    <!-- Activity bar — full height, far left -->
    <ActivityBar />

    <!-- Right of activity bar: header on top, content below -->
    <div class="app-shell__right">
      <AppHeader />

      <div class="app-shell__body">
        <!-- Side panel (sidebar panels only) -->
        <aside
          v-if="sidebarPanel"
          class="app-shell__side-panel"
          :style="{ width: sidePanelWidth + 'px' }"
        >
          <SchemaBrowser v-if="sidebarPanel === 'schema'" />
          <KeysBrowser v-if="sidebarPanel === 'keys'" />
          <ImportPanel v-if="sidebarPanel === 'import'" />
          <ExamplesPanel v-if="sidebarPanel === 'examples'" />
          <SettingsPanel v-if="sidebarPanel === 'settings'" />
        </aside>

        <!-- Side panel resize handle -->
        <div
          v-if="sidebarPanel"
          class="fg-resizer fg-resizer--v"
          :class="{ 'fg-resizer--active': isResizingSidePanel }"
          @mousedown="startResizeSidePanel"
        />

        <!-- Main content: full-width panels or editor+results -->
        <div class="app-shell__main">
          <template v-if="mainPanel">
            <DashboardPanel v-if="mainPanel === 'dashboard'" />
            <BenchmarkPanel v-if="mainPanel === 'benchmark'" />
            <QuickSearchPanel v-if="mainPanel === 'quicksearch'" />
            <SearchRefPanel v-if="mainPanel === 'reference'" />
            <DocsPanel v-if="mainPanel === 'docs'" />
          </template>
          <template v-else>
            <!-- Editor pane -->
            <div class="app-shell__editor" :style="{ height: editorHeight + 'px' }">
              <QueryEditor />
            </div>

            <!-- Editor/Result resize handle -->
            <div
              class="fg-resizer fg-resizer--h"
              :class="{ 'fg-resizer--active': isResizingEditor }"
              @mousedown="startResizeEditor"
            />

            <!-- Result pane -->
            <div class="app-shell__results">
              <ResultPanel />
            </div>
          </template>
        </div>

        <!-- AI Chat panel (right side) -->
        <template v-if="settings.aiChatOpen && conn.aiChatAvailable">
          <div
            class="fg-resizer fg-resizer--v"
            :class="{ 'fg-resizer--active': isResizingChatPanel }"
            @mousedown="startResizeChatPanel"
          />
          <aside
            class="app-shell__chat-panel"
            :style="{ width: chatPanelWidth + 'px' }"
          >
            <AiChatPanel />
          </aside>
        </template>
      </div>
    </div>

    <AppToast />
  </div>
</template>

<style scoped>
.app-shell {
  display: flex;
  height: 100%;
  width: 100%;
  overflow: hidden;
}

.app-shell__right {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-width: 0;
  height: 100%;
  overflow: hidden;
}

.app-shell__body {
  display: flex;
  flex: 1;
  min-height: 0;
  overflow: hidden;
}

.app-shell__side-panel {
  flex-shrink: 0;
  height: 100%;
  overflow: hidden;
  background: var(--tv-bg-surface);
}

.app-shell__main {
  flex: 1;
  min-width: 0;
  height: 100%;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.app-shell__editor {
  flex-shrink: 0;
  overflow: hidden;
  position: relative;
}

.app-shell__results {
  flex: 1;
  min-height: 0;
  overflow: hidden;
}

.app-shell__chat-panel {
  flex-shrink: 0;
  height: 100%;
  overflow: hidden;
}
</style>
