import { defineStore } from 'pinia'
import { takeSharedQuery } from '@/utils/share'
import { ref, computed } from 'vue'
import * as api from '@/utils/api'
import { useConnectionStore } from '@/stores/connection'

export interface QueryHistoryEntry {
  query: string
  collection: string
  timeMs: number
  found: number
  timestamp: number
}

export const useQueryStore = defineStore('query', () => {
  const sharedQuery = takeSharedQuery()
  const currentQuery = ref(sharedQuery ?? '{\n  "q": "*",\n  "query_by": "*",\n  "per_page": 10\n}')
  // A share link's query stands: the draft loaded when Studio opens would replace it.
  let keepSharedQuery = sharedQuery !== null
  const results = ref<any>(null)
  const isExecuting = ref(false)
  const error = ref<string | null>(null)
  const timeMs = ref(0)
  const resultViewMode = ref<'table' | 'json'>('table')
  const queryHistory = ref<QueryHistoryEntry[]>(loadHistory())
  const savedQueries = ref<{ name: string; query: string }[]>([])

  function loadHistory(): QueryHistoryEntry[] {
    try {
      return JSON.parse(localStorage.getItem('tq-query-history') || '[]')
    } catch { return [] }
  }

  function saveHistory() {
    const trimmed = queryHistory.value.slice(0, 100)
    localStorage.setItem('tq-query-history', JSON.stringify(trimmed))
  }

  function loadDraft(collection: string) {
    if (keepSharedQuery) {
      keepSharedQuery = false
      return
    }
    const draft = localStorage.getItem(`tq-query-draft-${collection}`)
    if (draft) { currentQuery.value = draft; return }
    const conn = useConnectionStore()
    const col = conn.collectionList.find((c: any) => c.name === collection)
    const textFields = (col?.fields || [])
      .filter((f: any) => (f.type === 'string' || f.type === 'string[]') && f.index !== false)
      .map((f: any) => f.name)
    currentQuery.value = JSON.stringify({ q: '*', query_by: '*', per_page: 10 }, null, 2)
  }

  function saveDraft(collection: string) {
    localStorage.setItem(`tq-query-draft-${collection}`, currentQuery.value)
  }

  async function executeQuery(collection: string) {
    if (!collection) {
      error.value = 'No collection selected'
      return
    }
    isExecuting.value = true
    error.value = null
    const start = performance.now()
    try {
      const parsed = JSON.parse(currentQuery.value)
      const data = await api.search(collection, parsed)
      timeMs.value = performance.now() - start
      results.value = data

      queryHistory.value.unshift({
        query: currentQuery.value,
        collection,
        timeMs: timeMs.value,
        found: data.found || 0,
        timestamp: Date.now(),
      })
      saveHistory()
    } catch (e: any) {
      timeMs.value = performance.now() - start
      if (e instanceof SyntaxError) {
        error.value = `Invalid JSON: ${e.message}`
      } else {
        error.value = e.message || 'Search failed'
      }
      results.value = null
    } finally {
      isExecuting.value = false
    }
  }

  async function fetchSavedQueries() {
    try {
      savedQueries.value = await api.savedQueries()
    } catch {}
  }

  async function saveCurrentQuery(name: string) {
    await api.saveQuery({ name, query: currentQuery.value })
    await fetchSavedQueries()
  }

  async function removeSavedQuery(name: string) {
    await api.deleteSavedQuery(name)
    await fetchSavedQueries()
  }

  const hits = computed(() => {
    if (!results.value) return []
    if (results.value.hits) return results.value.hits
    if (results.value.grouped_hits) {
      return results.value.grouped_hits.flatMap((g: any) => g.hits || [])
    }
    return []
  })

  return {
    currentQuery, results, isExecuting, error, timeMs,
    resultViewMode, queryHistory, savedQueries, hits,
    loadDraft, saveDraft, executeQuery,
    fetchSavedQueries, saveCurrentQuery, removeSavedQuery,
  }
})
