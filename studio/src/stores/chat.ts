import { defineStore } from 'pinia'
import { ref, computed, watch } from 'vue'
import { useConnectionStore } from './connection'
import { useQueryStore } from './query'
import { streamAiChat, type AiChatMessage, type AiChatMetadata } from '../utils/api'

const STORAGE_KEY = 'tq_chat_conversations'

export interface ChatMessage {
  role: 'user' | 'assistant'
  content: string
}

function loadConversations(): Map<string, ChatMessage[]> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return new Map()
    const entries: [string, ChatMessage[]][] = JSON.parse(raw)
    return new Map(entries)
  } catch {
    return new Map()
  }
}

function saveConversations(map: Map<string, ChatMessage[]>) {
  try {
    // Only persist conversations that have content
    const entries = Array.from(map.entries()).filter(([, msgs]) => msgs.length > 0)
    localStorage.setItem(STORAGE_KEY, JSON.stringify(entries))
  } catch {
    // localStorage quota exceeded — ignore
  }
}

export const useChatStore = defineStore('chat', () => {
  const conn = useConnectionStore()
  const query = useQueryStore()

  // Per-collection conversations (loaded from localStorage)
  const conversations = ref<Map<string, ChatMessage[]>>(loadConversations())
  const isStreaming = ref(false)
  const streamError = ref<string | null>(null)

  let abortController: AbortController | null = null

  const messages = computed(() => {
    return conversations.value.get(conn.currentCollection) ?? []
  })

  function ensureConversation(): ChatMessage[] {
    const col = conn.currentCollection
    if (!conversations.value.has(col)) {
      conversations.value.set(col, [])
    }
    return conversations.value.get(col)!
  }

  function buildMetadata(): AiChatMetadata {
    // Build collection schema string from collection list
    const lines: string[] = []
    const col = conn.collectionList.find((c: any) => c.name === conn.currentCollection)
    if (col?.fields) {
      lines.push(`Collection: ${col.name}`)
      lines.push('Fields:')
      for (const f of col.fields) {
        const attrs = []
        if (f.facet) attrs.push('facet')
        if (f.index === false) attrs.push('no-index')
        if (f.sort) attrs.push('sort')
        if (f.optional) attrs.push('optional')
        lines.push(`  ${f.name}: ${f.type}${attrs.length ? ` (${attrs.join(', ')})` : ''}`)
      }
    }

    const metadata: AiChatMetadata = {
      collectionSchema: lines.join('\n'),
    }

    if (query.currentQuery) {
      metadata.currentQuery = query.currentQuery
    }
    if (query.timeMs > 0) {
      metadata.lastQueryTimeMs = query.timeMs
    }

    return metadata
  }

  async function sendMessage(content: string) {
    const msgs = ensureConversation()
    msgs.push({ role: 'user', content })
    msgs.push({ role: 'assistant', content: '' })

    // Trigger reactivity
    conversations.value = new Map(conversations.value)

    isStreaming.value = true
    streamError.value = null
    abortController = new AbortController()

    const apiMessages: AiChatMessage[] = msgs
      .filter((m) => m.role === 'user' || (m.role === 'assistant' && m.content))
      .map((m) => ({ role: m.role, content: m.content }))

    const metadata = buildMetadata()

    try {
      const assistantMsg = msgs[msgs.length - 1]
      await streamAiChat(
        apiMessages,
        metadata,
        (delta) => {
          assistantMsg.content += delta
          // Trigger reactivity
          conversations.value = new Map(conversations.value)
        },
        abortController.signal,
      )
    } catch (e: unknown) {
      if (e instanceof DOMException && e.name === 'AbortError') {
        // User cancelled — keep partial response
      } else {
        const errMsg = e instanceof Error ? e.message : 'Unknown error'
        streamError.value = errMsg
        // Remove the empty assistant message if nothing was streamed
        const last = msgs[msgs.length - 1]
        if (last.role === 'assistant' && !last.content) {
          msgs.pop()
          conversations.value = new Map(conversations.value)
        }
      }
    } finally {
      isStreaming.value = false
      abortController = null
    }
  }

  function stopStreaming() {
    abortController?.abort()
  }

  function clearConversation() {
    conversations.value.set(conn.currentCollection, [])
    conversations.value = new Map(conversations.value)
    streamError.value = null
  }

  // Persist to localStorage whenever conversations change
  watch(conversations, (map) => saveConversations(map), { deep: true })

  return {
    messages,
    isStreaming,
    streamError,
    sendMessage,
    stopStreaming,
    clearConversation,
  }
})
