<script setup lang="ts">
import { ref, onMounted, onUnmounted, watch } from 'vue'
import { useQueryStore } from '../../stores/query'
import { useConnectionStore } from '../../stores/connection'
import { useSettingsStore } from '../../stores/settings'
import { useTheme } from '../../composables/useTheme'
import { useToast } from '../../composables/useToast'
import { registerCompletionProvider, type FieldInfo } from './SearchLanguage'
import QueryPalette from './QueryPalette.vue'
import SaveQueryDialog from './SaveQueryDialog.vue'
import ShareQueryDialog from './ShareQueryDialog.vue'
import type * as Monaco from 'monaco-editor'

// Monaco web worker setup — must be configured before importing monaco-editor
import editorWorker from 'monaco-editor/esm/vs/editor/editor.worker?worker'
import jsonWorker from 'monaco-editor/esm/vs/language/json/json.worker?worker'
import cssWorker from 'monaco-editor/esm/vs/language/css/css.worker?worker'
import htmlWorker from 'monaco-editor/esm/vs/language/html/html.worker?worker'
import tsWorker from 'monaco-editor/esm/vs/language/typescript/ts.worker?worker'

self.MonacoEnvironment = {
  getWorker(_: unknown, label: string) {
    if (label === 'json') return new jsonWorker()
    if (label === 'css' || label === 'scss' || label === 'less') return new cssWorker()
    if (label === 'html' || label === 'handlebars' || label === 'razor') return new htmlWorker()
    if (label === 'typescript' || label === 'javascript') return new tsWorker()
    return new editorWorker()
  },
}

const queryStore = useQueryStore()
const settings = useSettingsStore()
const conn = useConnectionStore()
const { theme } = useTheme()
const toast = useToast()

const editorContainer = ref<HTMLElement | null>(null)
let editor: Monaco.editor.IStandaloneCodeEditor | null = null
let monaco: typeof Monaco | null = null
let completionDisposable: Monaco.IDisposable | null = null
const showPalette = ref(false)
const showSaveDialog = ref(false)
const showShareDialog = ref(false)

function getCollectionFields(): FieldInfo[] {
  const col = conn.collectionList.find((c: any) => c.name === conn.currentCollection)
  return (col?.fields || []) as FieldInfo[]
}

onMounted(async () => {
  // Dynamic import to avoid bundling monaco in main chunk
  monaco = await import('monaco-editor')

  if (!editorContainer.value) return

  completionDisposable = registerCompletionProvider(monaco, getCollectionFields)

  // Define custom themes matching Studio palette
  monaco.editor.defineTheme('tq-dark', {
    base: 'vs-dark',
    inherit: true,
    rules: [
      { token: 'string.key.json', foreground: '38BDF8' },
      { token: 'string.value.json', foreground: 'D4A030' },
      { token: 'number', foreground: '5B9BD5' },
      { token: 'keyword', foreground: 'a78bfa' },
    ],
    colors: {
      'editor.background': '#151D24',
      'editor.foreground': '#C9CDCF',
      'editor.lineHighlightBackground': '#1C262F',
      'editor.selectionBackground': '#0EA5E920',
      'editor.inactiveSelectionBackground': '#0EA5E910',
      'editorCursor.foreground': '#38BDF8',
      'editorLineNumber.foreground': '#3A4A58',
      'editorLineNumber.activeForeground': '#7E8B97',
      'editorGutter.background': '#151D24',
      'editorWidget.background': '#1C262F',
      'editorWidget.border': '#2A3A48',
      'input.background': '#0E1419',
      'input.border': '#2A3A48',
      'scrollbarSlider.background': '#2A3A4880',
      'scrollbarSlider.hoverBackground': '#3A4A5880',
    },
  })
  monaco.editor.defineTheme('tq-light', {
    base: 'vs',
    inherit: true,
    rules: [
      { token: 'string.key.json', foreground: '0369A1' },
      { token: 'string.value.json', foreground: 'B8860B' },
      { token: 'number', foreground: '2A6BB5' },
      { token: 'keyword', foreground: '7C3AED' },
    ],
    colors: {
      'editor.background': '#FFFFFF',
      'editor.foreground': '#1A1D20',
      'editor.lineHighlightBackground': '#F3F4F5',
      'editor.selectionBackground': '#0369A118',
      'editor.inactiveSelectionBackground': '#0369A10A',
      'editorCursor.foreground': '#0369A1',
      'editorLineNumber.foreground': '#B8BEC6',
      'editorLineNumber.activeForeground': '#5A6370',
      'editorGutter.background': '#FFFFFF',
      'editorWidget.background': '#FFFFFF',
      'editorWidget.border': '#D8DBDF',
      'input.background': '#F3F4F5',
      'input.border': '#D8DBDF',
      'scrollbarSlider.background': '#D8DBDF80',
      'scrollbarSlider.hoverBackground': '#B8BEC680',
    },
  })

  editor = monaco.editor.create(editorContainer.value, {
    value: queryStore.currentQuery,
    language: 'json',
    theme: theme.value === 'dark' ? 'tq-dark' : 'tq-light',
    fontSize: settings.editorFontSize,
    minimap: { enabled: false },
    lineNumbers: 'on',
    wordWrap: 'on',
    scrollBeyondLastLine: false,
    automaticLayout: true,
    tabSize: 2,
    renderWhitespace: 'none',
    overviewRulerLanes: 0,
    hideCursorInOverviewRuler: true,
    scrollbar: {
      verticalScrollbarSize: 8,
      horizontalScrollbarSize: 8,
    },
    padding: { top: 8, bottom: 8 },
    quickSuggestions: true,
    suggestOnTriggerCharacters: true,
    wordBasedSuggestions: 'off',
  })

  // Ctrl+Enter to execute
  editor.addAction({
    id: 'execute-query',
    label: 'Execute Query',
    keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter],
    run: () => {
      queryStore.currentQuery = editor!.getValue()
      queryStore.executeQuery(conn.currentCollection)
    },
  })

  // Sync editor content to store
  editor.onDidChangeModelContent(() => {
    queryStore.currentQuery = editor!.getValue()
    if (conn.currentCollection) {
      queryStore.saveDraft(conn.currentCollection)
    }
  })

  // Load saved queries from server (skip if auth gate is showing)
  if (!conn.authRequired) queryStore.fetchSavedQueries()
})

// Sync store → editor (for external updates like Examples "Use" button)
watch(() => queryStore.currentQuery, (v) => {
  if (editor && v !== editor.getValue()) {
    editor.setValue(v)
  }
})

// Watch theme changes
watch(theme, (t) => {
  if (monaco && editor) {
    monaco.editor.setTheme(t === 'dark' ? 'tq-dark' : 'tq-light')
  }
})

// Watch font size changes
watch(() => settings.editorFontSize, (size) => {
  editor?.updateOptions({ fontSize: size })
})

// Re-register completions when collection changes (fields may differ)
watch(() => conn.currentCollection, () => {
  if (!monaco) return
  completionDisposable?.dispose()
  completionDisposable = registerCompletionProvider(monaco, getCollectionFields)
})

// Global keyboard shortcuts — must use window listener to prevent browser defaults
function onGlobalKeydown(e: KeyboardEvent) {
  const mod = e.ctrlKey || e.metaKey
  if (!mod) return

  if ((e.key === 's' || e.key === 'S') && e.shiftKey) {
    e.preventDefault()
    const q = editor?.getValue()?.trim() ?? ''
    if (!q) {
      toast.error('Nothing to share')
      return
    }
    showShareDialog.value = true
  } else if (e.key === 's' || e.key === 'S') {
    e.preventDefault()
    const q = editor?.getValue()?.trim() ?? ''
    if (!q) {
      toast.error('Nothing to save')
      return
    }
    showSaveDialog.value = true
  } else if (e.key === 'p' || e.key === 'P') {
    e.preventDefault()
    showPalette.value = !showPalette.value
  }
}

window.addEventListener('keydown', onGlobalKeydown)

onUnmounted(() => {
  window.removeEventListener('keydown', onGlobalKeydown)
  completionDisposable?.dispose()
  editor?.dispose()
})

function execute() {
  queryStore.currentQuery = editor?.getValue() ?? ''
  queryStore.executeQuery(conn.currentCollection)
}

function handleSave(name: string) {
  const q = editor?.getValue()?.trim() ?? ''
  if (!q) return
  const finalName = name || `query_${new Date().toTimeString().slice(0, 8).replace(/:/g, '')}`
  queryStore.saveCurrentQuery(finalName)
  toast.success(`Saved as '${finalName}'`)
  showSaveDialog.value = false
}

function handlePaletteSelect(payload: { query: string }) {
  if (editor) {
    editor.setValue(payload.query)
    queryStore.currentQuery = payload.query
  }
  showPalette.value = false
}
</script>

<template>
  <div class="query-editor">
    <div class="query-editor__editor-wrap">
      <div ref="editorContainer" class="query-editor__monaco" />

      <!-- Floating Run button -->
      <button
        class="query-editor__run-btn"
        :disabled="queryStore.isExecuting"
        @click="execute"
        title="Execute query"
      >
        <span v-if="queryStore.isExecuting" class="fg-spinner" style="width:12px;height:12px;border-width:1.5px" />
        <svg v-else width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
          <path d="m6 3 14 9-14 9V3z" />
        </svg>
        <span class="query-editor__run-label">Ctrl+Enter</span>
      </button>

      <!-- Keyboard shortcut hints (clickable) -->
      <div class="query-editor__hints">
        <button class="query-editor__hint" @click="showSaveDialog = true"><kbd>Ctrl+S</kbd> Save</button>
        <button class="query-editor__hint" @click="showShareDialog = true"><kbd>Ctrl+Shift+S</kbd> Share</button>
        <button class="query-editor__hint" @click="showPalette = !showPalette"><kbd>Ctrl+P</kbd> Queries</button>
      </div>

      <QueryPalette v-if="showPalette" @close="showPalette = false" @select="handlePaletteSelect" />
    </div>

    <SaveQueryDialog
      v-if="showSaveDialog"
      @save="handleSave"
      @cancel="showSaveDialog = false"
    />

    <ShareQueryDialog
      v-if="showShareDialog"
      :query="queryStore.currentQuery.trim()"
      @close="showShareDialog = false"
    />
  </div>
</template>

<style scoped>
.query-editor {
  display: flex;
  flex-direction: column;
  height: 100%;
  background: var(--tv-bg-surface);
  position: relative;
}

.query-editor__editor-wrap {
  flex: 1;
  min-height: 0;
  position: relative;
}

.query-editor__monaco {
  width: 100%;
  height: 100%;
}

/* Floating Run button */
.query-editor__run-btn {
  position: absolute;
  bottom: 12px;
  right: 16px;
  z-index: 20;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 6px 14px;
  height: 32px;
  border: none;
  border-radius: 9999px;
  background: var(--tv-accent);
  color: var(--tv-text-inverse);
  font-family: var(--tv-font-sans);
  font-size: var(--tv-font-size-xs);
  font-weight: 500;
  cursor: pointer;
  opacity: 0.75;
  transition: opacity 0.15s, background 0.15s;
  box-shadow: var(--tv-shadow-md);
}
.query-editor__run-btn:hover {
  opacity: 1;
  background: var(--tv-accent-hover);
}
.query-editor__run-btn:active {
  background: var(--tv-accent-active);
}
.query-editor__run-btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.query-editor__run-label {
  font-size: 11px;
  opacity: 0.85;
}

/* Keyboard shortcut hints */
.query-editor__hints {
  position: absolute;
  bottom: 14px;
  left: 12px;
  z-index: 20;
  display: flex;
  gap: 4px;
  opacity: 0.4;
  transition: opacity 0.15s;
}
.query-editor__editor-wrap:hover .query-editor__hints {
  opacity: 0.7;
}

.query-editor__hint {
  font-family: var(--tv-font-sans);
  font-size: 11px;
  color: var(--tv-text-secondary);
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 3px 8px;
  border: none;
  border-radius: var(--tv-radius-sm);
  background: none;
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.query-editor__hint:hover {
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}

.query-editor__hint kbd {
  font-family: var(--tv-font-sans);
  font-size: 10px;
  padding: 1px 5px;
  border: 1px solid var(--tv-border-default);
  border-radius: 3px;
  background: var(--tv-bg-elevated);
  color: var(--tv-text-secondary);
}
</style>
