import { defineStore } from 'pinia'
import { ref } from 'vue'
import * as api from '@/utils/api'

export const useSchemaStore = defineStore('schema', () => {
  const collections = ref<any[]>([])
  const currentSchema = ref<any>(null)

  async function fetchCollections() {
    try {
      collections.value = await api.collections()
    } catch {}
  }

  async function fetchSchema(name: string) {
    try {
      currentSchema.value = await api.getCollection(name)
    } catch {
      currentSchema.value = null
    }
  }

  return { collections, currentSchema, fetchCollections, fetchSchema }
})
