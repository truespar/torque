const API_KEY_STORAGE = 'tq-studio-api-key'

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message)
  }
}

export function getApiKey(): string | null {
  return localStorage.getItem(API_KEY_STORAGE)
}

export function setApiKey(key: string) {
  localStorage.setItem(API_KEY_STORAGE, key)
}

export function clearApiKey() {
  localStorage.removeItem(API_KEY_STORAGE)
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { 'Content-Type': 'application/json' }
  const key = getApiKey()
  if (key) headers['X-TYPESENSE-API-KEY'] = key

  const resp = await fetch(path, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  })

  if (!resp.ok) {
    const text = await resp.text()
    let msg = text
    try { msg = JSON.parse(text).message || text } catch {}
    throw new ApiError(resp.status, msg)
  }

  const ct = resp.headers.get('content-type') || ''
  if (ct.includes('application/json')) return resp.json()
  return resp.text() as unknown as T
}

async function requestRaw(method: string, path: string, body?: string, contentType?: string): Promise<string> {
  const headers: Record<string, string> = {}
  if (contentType) headers['Content-Type'] = contentType
  const key = getApiKey()
  if (key) headers['X-TYPESENSE-API-KEY'] = key

  const resp = await fetch(path, { method, headers, body })
  if (!resp.ok) {
    const text = await resp.text()
    let msg = text
    try { msg = JSON.parse(text).message || text } catch {}
    throw new ApiError(resp.status, msg)
  }
  return resp.text()
}

// --- Health ---
export const health = () => request<any>('GET', '/health')
export const healthReady = () => request<any>('GET', '/health/ready')
export const adminStats = () => request<any>('GET', '/admin/stats')

// --- Collections ---
export const collections = () => request<any[]>('GET', '/collections')
export const createCollection = (schema: any) => request<any>('POST', '/collections', schema)
export const getCollection = (name: string) => request<any>('GET', `/collections/${encodeURIComponent(name)}`)
export const updateCollection = (name: string, body: any) => request<any>('PATCH', `/collections/${encodeURIComponent(name)}`, body)
export const deleteCollection = (name: string) => request<any>('DELETE', `/collections/${encodeURIComponent(name)}`)

// --- Documents ---
export function search(collection: string, params: Record<string, any>) {
  const qs = new URLSearchParams()
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== null && v !== '') qs.set(k, String(v))
  }
  return request<any>('GET', `/collections/${encodeURIComponent(collection)}/documents/search?${qs}`)
}

export const multiSearch = (body: any) => request<any>('POST', '/multi_search', body)

export function importDocuments(collection: string, jsonl: string, action: string = 'upsert') {
  return requestRaw(
    'POST',
    `/collections/${encodeURIComponent(collection)}/documents/import?action=${action}`,
    jsonl,
    'text/plain',
  )
}

export function exportDocuments(collection: string) {
  return requestRaw('GET', `/collections/${encodeURIComponent(collection)}/documents/export`)
}

export const compactCollection = (name: string) =>
  request<any>('POST', `/collections/${encodeURIComponent(name)}/documents/compact`)

export const getDocument = (col: string, id: string) =>
  request<any>('GET', `/collections/${encodeURIComponent(col)}/documents/${encodeURIComponent(id)}`)

export const createDocument = (col: string, doc: any) =>
  request<any>('POST', `/collections/${encodeURIComponent(col)}/documents`, doc)

export const updateDocument = (col: string, id: string, doc: any) =>
  request<any>('PATCH', `/collections/${encodeURIComponent(col)}/documents/${encodeURIComponent(id)}`, doc)

export const deleteDocument = (col: string, id: string) =>
  request<any>('DELETE', `/collections/${encodeURIComponent(col)}/documents/${encodeURIComponent(id)}`)

// --- Aliases ---
export const aliases = () => request<any>('GET', '/aliases')
export const createAlias = (name: string, body: any) => request<any>('PUT', `/aliases/${encodeURIComponent(name)}`, body)
export const getAlias = (name: string) => request<any>('GET', `/aliases/${encodeURIComponent(name)}`)
export const deleteAlias = (name: string) => request<any>('DELETE', `/aliases/${encodeURIComponent(name)}`)

// --- Synonyms ---
export const synonymSets = () => request<any>('GET', '/synonym_sets')
export const createSynonymSet = (name: string, body: any) => request<any>('PUT', `/synonym_sets/${encodeURIComponent(name)}`, body)
export const getSynonymSet = (name: string) => request<any>('GET', `/synonym_sets/${encodeURIComponent(name)}`)
export const deleteSynonymSet = (name: string) => request<any>('DELETE', `/synonym_sets/${encodeURIComponent(name)}`)
export const synonymItems = (set: string) => request<any>('GET', `/synonym_sets/${encodeURIComponent(set)}/items`)
export const createSynonymItem = (set: string, id: string, body: any) =>
  request<any>('PUT', `/synonym_sets/${encodeURIComponent(set)}/items/${encodeURIComponent(id)}`, body)
export const getSynonymItem = (set: string, id: string) =>
  request<any>('GET', `/synonym_sets/${encodeURIComponent(set)}/items/${encodeURIComponent(id)}`)
export const deleteSynonymItem = (set: string, id: string) =>
  request<any>('DELETE', `/synonym_sets/${encodeURIComponent(set)}/items/${encodeURIComponent(id)}`)

// --- Presets ---
export const presets = () => request<any>('GET', '/presets')
export const createPreset = (id: string, body: any) => request<any>('PUT', `/presets/${encodeURIComponent(id)}`, body)
export const getPreset = (id: string) => request<any>('GET', `/presets/${encodeURIComponent(id)}`)
export const deletePreset = (id: string) => request<any>('DELETE', `/presets/${encodeURIComponent(id)}`)

// --- Stopwords ---
export const stopwords = () => request<any>('GET', '/stopwords')
export const createStopwordSet = (id: string, body: any) => request<any>('PUT', `/stopwords/${encodeURIComponent(id)}`, body)
export const getStopwordSet = (id: string) => request<any>('GET', `/stopwords/${encodeURIComponent(id)}`)
export const deleteStopwordSet = (id: string) => request<any>('DELETE', `/stopwords/${encodeURIComponent(id)}`)

// --- API Keys ---
export const keys = () => request<any>('GET', '/keys')
export const createKey = (body: any) => request<any>('POST', '/keys', body)
export const getKey = (id: string) => request<any>('GET', `/keys/${encodeURIComponent(id)}`)
export const deleteKey = (id: string) => request<any>('DELETE', `/keys/${encodeURIComponent(id)}`)

// --- Studio saved queries ---
export const savedQueries = () => request<any[]>('GET', '/api/studio/queries')
export const saveQuery = (body: { name: string; query: string }) => request<any>('POST', '/api/studio/queries', body)
export const deleteSavedQuery = (name: string) => request<any>('DELETE', `/api/studio/queries/${encodeURIComponent(name)}`)

// --- AI Chat ---
// The Search Assistant, answered by the model the server is configured with (`ai_provider`;
// health reports `features.ai`). The API key stays on the server.
export interface AiChatMessage {
  role: 'user' | 'assistant'
  content: string
}

export interface AiChatMetadata {
  collectionSchema: string
  currentQuery?: string
  lastQueryTimeMs?: number
}

export async function streamAiChat(
  messages: AiChatMessage[],
  metadata: AiChatMetadata,
  onDelta: (text: string) => void,
  signal?: AbortSignal,
): Promise<void> {
  const apiKey = getApiKey()
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'text/event-stream',
  }
  if (apiKey) headers['X-TYPESENSE-API-KEY'] = apiKey

  const res = await fetch('/api/ai/chat', {
    method: 'POST',
    headers,
    body: JSON.stringify({ messages, metadata }),
    signal,
  })

  if (!res.ok) {
    if (res.status === 401) clearApiKey()
    let message = res.statusText
    try {
      const body = await res.json()
      if (body.error) message = body.error
    } catch { /* ignore */ }
    throw new ApiError(res.status, message)
  }

  const reader = res.body?.getReader()
  if (!reader) return

  const decoder = new TextDecoder()
  let buffer = ''

  while (true) {
    const { done, value } = await reader.read()
    if (done) break

    buffer += decoder.decode(value, { stream: true })
    const lines = buffer.split('\n')
    buffer = lines.pop() ?? ''

    for (const line of lines) {
      if (!line.startsWith('data: ')) continue
      const data = line.slice(6)
      if (data === '[DONE]') return

      let event
      try {
        event = JSON.parse(data)
      } catch {
        continue // not JSON: skip the line
      }
      if (event.type === 'text') {
        onDelta(event.text)
      } else if (event.type === 'error') {
        // why the answer stopped (provider error, refusal, cut-off)
        throw new Error(event.error || 'Stream error')
      }
    }
  }
}
