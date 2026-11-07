import { ref, watch } from 'vue'
import { useSettingsStore } from '@/stores/settings'

export type Theme = 'dark' | 'light'

const theme = ref<Theme>((localStorage.getItem('tq-theme') as Theme) || 'dark')

export function useTheme() {
  function applyTheme(t: Theme) {
    document.documentElement.setAttribute('data-theme', t)
    localStorage.setItem('tq-theme', t)
    // Sync to settings store so components like AiChatPanel can read it
    try { useSettingsStore().theme = t } catch { /* store not yet ready */ }
  }

  function toggleTheme() {
    theme.value = theme.value === 'dark' ? 'light' : 'dark'
  }

  function initTheme() {
    applyTheme(theme.value)
  }

  watch(theme, applyTheme, { immediate: true })

  return { theme, toggleTheme, initTheme }
}
