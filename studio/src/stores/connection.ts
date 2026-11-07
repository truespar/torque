import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import * as api from '@/utils/api'

export const useConnectionStore = defineStore('connection', () => {
  const isConnected = ref(false)
  const authRequired = ref(true)
  const initialCheckDone = ref(false)
  const collectionList = ref<any[]>([])
  const currentCollection = ref<string>(localStorage.getItem('tq-current-collection') || '')
  const version = ref('')
  const memory = ref<{ allocated_bytes: number; resident_bytes: number } | null>(null)
  const uptime = ref(0)
  const collectionStats = ref<{ name: string; num_documents: number; memory?: any }[]>([])
  const allowDropCollection = ref(true)
  // The Search Assistant needs a model on the server (`ai_provider`).
  const aiChatAvailable = ref(false)

  let pollTimer: ReturnType<typeof setInterval> | null = null

  const collectionNames = computed(() => collectionList.value.map((c: any) => c.name))

  async function pollHealth() {
    try {
      const h = await api.health()
      isConnected.value = true
      if (h.version) version.value = h.version
      aiChatAvailable.value = h.features?.ai === true
      if (h.allow_drop_collection !== undefined) {
        allowDropCollection.value = !!h.allow_drop_collection
      }

      // Health is unauthenticated — check if we have a valid key by
      // trying an authenticated endpoint. If no key stored, skip.
      const key = api.getApiKey()
      if (!key) {
        authRequired.value = true
        return
      }

      try {
        const stats = await api.adminStats()
        if (stats.system?.memory) memory.value = stats.system.memory
        if (stats.system?.uptime_seconds) uptime.value = stats.system.uptime_seconds
        if (stats.collections) collectionStats.value = stats.collections
        authRequired.value = false
      } catch (e: any) {
        // Any failure on an authenticated endpoint means the stored key
        // can't be verified — force the user back to the login screen.
        // On 401/403 also clear the key from localStorage so the old
        // bad key isn't silently reused on the next attempt.
        authRequired.value = true
        if (e instanceof api.ApiError && (e.status === 401 || e.status === 403)) {
          api.clearApiKey()
        }
      }
    } catch {
      isConnected.value = false
    }
  }

  async function fetchCollections() {
    try {
      collectionList.value = await api.collections()
      // Clear stale selection if the collection no longer exists
      if (currentCollection.value && !collectionList.value.some((c: any) => c.name === currentCollection.value)) {
        setCurrentCollection('')
      }
    } catch {}
  }

  function setCurrentCollection(name: string) {
    currentCollection.value = name
    localStorage.setItem('tq-current-collection', name)
  }

  async function startPolling() {
    await pollHealth()
    initialCheckDone.value = true
    if (!authRequired.value) fetchCollections()
    pollTimer = setInterval(() => {
      pollHealth()
      if (!authRequired.value) fetchCollections()
    }, 5000)
  }

  function stopPolling() {
    if (pollTimer) clearInterval(pollTimer)
  }

  return {
    isConnected, authRequired, initialCheckDone, collectionList, currentCollection, aiChatAvailable,
    version, memory, uptime, collectionNames, collectionStats,
    allowDropCollection,
    pollHealth, fetchCollections, setCurrentCollection, startPolling, stopPolling,
  }
})
