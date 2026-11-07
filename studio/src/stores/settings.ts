import { defineStore } from 'pinia'
import { ref, watch } from 'vue'

export type PanelId = 'dashboard' | 'quicksearch' | 'benchmark' | 'schema' | 'keys' | 'import' | 'examples' | 'reference' | 'docs' | 'settings'

export const useSettingsStore = defineStore('settings', () => {
  const activePanel = ref<PanelId | null>('benchmark')
  const aiChatOpen = ref(false)
  const editorFontSize = ref(Number(localStorage.getItem('tq-editor-font-size')) || 13)
  const theme = ref<'dark' | 'light'>((localStorage.getItem('tq-theme') as 'dark' | 'light') || 'dark')

  watch(editorFontSize, v => localStorage.setItem('tq-editor-font-size', String(v)))

  return { activePanel, aiChatOpen, editorFontSize, theme }
})
