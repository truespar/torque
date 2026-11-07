// Share links that carry the query themselves: base64url JSON in the URL fragment, which the
// browser never sends to a server. `readShareLink` runs before the stores start, so the link's
// collection is the one that opens and the query store finds the query.

/** What a share link carries. */
interface SharePayload {
  q: string
  c?: string
}

let shared: string | null = null

/** A link to this Studio that opens with `query` in the editor, on `collection` if set. */
export function shareUrl(query: string, collection?: string): string {
  const payload: SharePayload = { q: query }
  if (collection) payload.c = collection
  // URL-safe base64 (no `+` / `/`, no padding) survives chat and email clients that mangle those
  const b64 = btoa(unescape(encodeURIComponent(JSON.stringify(payload))))
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=+$/, '')
  return `${location.origin}${location.pathname}#q=${b64}`
}

/** Takes a share link out of the URL, so a refresh doesn't prime the editor again. */
export function readShareLink(): void {
  const m = location.hash.match(/[#&]q=([^&]+)/)
  if (!m) return
  try {
    const b64 = m[1].replace(/-/g, '+').replace(/_/g, '/')
    const padded = b64 + '='.repeat((4 - (b64.length % 4)) % 4)
    const payload = JSON.parse(decodeURIComponent(escape(atob(padded)))) as Partial<SharePayload>
    if (typeof payload.q === 'string' && payload.q) {
      shared = payload.q
      if (typeof payload.c === 'string' && payload.c) {
        localStorage.setItem('tq-current-collection', payload.c)
      }
    }
  } catch (err) {
    console.warn('Torque: failed to read the share link', err)
  }
  // keeps history.state, which is the router's
  history.replaceState(history.state, '', location.pathname + location.search)
}

/** The query of the share link this page opened with, once. */
export function takeSharedQuery(): string | null {
  const query = shared
  shared = null
  return query
}
