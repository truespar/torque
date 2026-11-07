import type * as Monaco from 'monaco-editor'

export interface FieldInfo {
  name: string
  type: string
  facet?: boolean
  sort?: boolean
  index?: boolean
}

// --- Field category helpers ---

function textFields(fields: FieldInfo[]): FieldInfo[] {
  return fields.filter(f => (f.type === 'string' || f.type === 'string[]') && f.index !== false)
}

function sortableFields(fields: FieldInfo[]): FieldInfo[] {
  return fields.filter(f => f.sort === true)
}

function facetableFields(fields: FieldInfo[]): FieldInfo[] {
  return fields.filter(f => f.facet === true)
}

function geoFields(fields: FieldInfo[]): FieldInfo[] {
  return fields.filter(f => f.type === 'geopoint' || f.type === 'geopoint[]')
}

function allIndexedFields(fields: FieldInfo[]): FieldInfo[] {
  return fields.filter(f => f.index !== false)
}

// --- Search request schema (for JSON validation + hover) ---

export const searchRequestSchema = {
  type: 'object',
  properties: {
    q: { type: 'string', description: 'Search query text. Use * for wildcard.' },
    query_by: { type: 'string', description: 'Comma-separated list of fields to search.' },
    filter_by: { type: 'string', description: 'Filter expression (e.g. "price:>100 && brand:=Nike").' },
    sort_by: { type: 'string', description: 'Sort expression (e.g. "price:asc,_text_match:desc").' },
    facet_by: { type: 'string', description: 'Comma-separated list of fields to facet on.' },
    max_facet_values: { type: 'integer', description: 'Maximum facet values per field (default: 10).' },
    per_page: { type: 'integer', description: 'Results per page (default: 10, max: 250).' },
    page: { type: 'integer', description: 'Page number (1-based).' },
    prefix: { type: ['boolean', 'string'], description: 'Enable prefix search (default: true).' },
    num_typos: { type: ['integer', 'string'], description: 'Typos allowed per query token (default: 2).' },
    query_by_weights: { type: 'string', description: 'Comma-separated weights for query_by fields.' },
    include_fields: { type: 'string', description: 'Comma-separated fields to include in results.' },
    exclude_fields: { type: 'string', description: 'Comma-separated fields to exclude from results.' },
    highlight_fields: { type: 'string', description: 'Fields to highlight in results.' },
    highlight_full_fields: { type: 'string', description: 'Fields for full highlight (no snippet).' },
    highlight_affix_num_tokens: { type: 'integer', description: 'Tokens before/after highlight (default: 4).' },
    highlight_start_tag: { type: 'string', description: 'HTML tag to start highlight (default: <mark>).' },
    highlight_end_tag: { type: 'string', description: 'HTML tag to end highlight (default: </mark>).' },
    group_by: { type: 'string', description: 'Field to group results by.' },
    group_limit: { type: 'integer', description: 'Max hits per group (default: 3).' },
    pinned_hits: { type: 'string', description: 'Comma-separated id:position pairs to pin.' },
    hidden_hits: { type: 'string', description: 'Comma-separated ids to hide from results.' },
    text_match_type: { type: 'string', description: 'Text match scoring mode: max_score, max_weight, or sum_score.' },
    vector_query: { type: 'string', description: 'Vector search: field:([v1,v2,...], k:N). Params: distance_threshold, ef, flat_search_cutoff, alpha. Find similar: field:(id:DOC_ID, k:N). Sort: _vector_distance:asc.' },
    preset: { type: 'string', description: 'Preset name to apply.' },
    stopwords: { type: 'string', description: 'Stopword set id to apply.' },
  },
}

const paramDocs: Record<string, string> = Object.fromEntries(
  Object.entries(searchRequestSchema.properties).map(([k, v]) => [k, v.description])
)

// --- Context detection ---

type EditorContext =
  | { kind: 'key' }
  | { kind: 'value'; key: string }
  | { kind: 'unknown' }

function detectContext(textBeforeCursor: string): EditorContext {
  // Check if we're inside a value for a known key: "key": "...cursor
  // Walk backwards to find the last "key": pattern before cursor
  const keyValueMatch = textBeforeCursor.match(/"(\w+)"\s*:\s*"[^"]*$/)
  if (keyValueMatch) {
    return { kind: 'value', key: keyValueMatch[1] }
  }

  // Check if cursor is at a key position (after { or , at top level)
  // i.e. we're about to type or are typing a key name
  const keyPosMatch = textBeforeCursor.match(/(?:^\s*\{|,)\s*"?\w*$/)
    || textBeforeCursor.match(/^\s*"?\w*$/m)
  if (keyPosMatch) {
    return { kind: 'key' }
  }

  return { kind: 'unknown' }
}

/** Get the current segment in a comma-separated value (for multi-field params) */
function currentSegment(fullValue: string): string {
  const parts = fullValue.split(',')
  return (parts[parts.length - 1] || '').trim()
}

// --- Completion generators ---

function keyCompletions(
  monaco: typeof Monaco,
  range: Monaco.IRange,
): Monaco.languages.CompletionItem[] {
  const items: Monaco.languages.CompletionItem[] = []

  for (const [key, desc] of Object.entries(paramDocs)) {
    items.push({
      label: key,
      kind: monaco.languages.CompletionItemKind.Property,
      detail: 'search parameter',
      documentation: desc,
      insertText: `"${key}": "$0"`,
      insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
      range,
      sortText: `0_${key}`,
    })
  }

  // Snippet templates at lower priority
  const snippets: Array<{ label: string; detail: string; body: string }> = [
    {
      label: 'Basic search',
      detail: 'template',
      body: [
        '{',
        '  "q": "$1",',
        '  "query_by": "$2",',
        '  "per_page": ${3:10}',
        '}',
      ].join('\n'),
    },
    {
      label: 'Filtered search',
      detail: 'template',
      body: [
        '{',
        '  "q": "$1",',
        '  "query_by": "$2",',
        '  "filter_by": "$3",',
        '  "per_page": ${4:10}',
        '}',
      ].join('\n'),
    },
    {
      label: 'Faceted search',
      detail: 'template',
      body: [
        '{',
        '  "q": "$1",',
        '  "query_by": "$2",',
        '  "facet_by": "$3",',
        '  "max_facet_values": ${4:10},',
        '  "per_page": ${5:10}',
        '}',
      ].join('\n'),
    },
    {
      label: 'Sorted search',
      detail: 'template',
      body: [
        '{',
        '  "q": "$1",',
        '  "query_by": "$2",',
        '  "sort_by": "$3",',
        '  "per_page": ${4:10}',
        '}',
      ].join('\n'),
    },
    {
      label: 'Grouped search',
      detail: 'template',
      body: [
        '{',
        '  "q": "$1",',
        '  "query_by": "$2",',
        '  "group_by": "$3",',
        '  "group_limit": ${4:3},',
        '  "per_page": ${5:10}',
        '}',
      ].join('\n'),
    },
    {
      label: 'Geo search',
      detail: 'template',
      body: [
        '{',
        '  "q": "*",',
        '  "query_by": "$1",',
        '  "filter_by": "${2:location}:(${3:lat}, ${4:lng}, ${5:10} km)",',
        '  "sort_by": "${2:location}(${3:lat}, ${4:lng}):asc",',
        '  "per_page": ${6:10}',
        '}',
      ].join('\n'),
    },
    {
      label: 'Vector search',
      detail: 'template',
      body: [
        '{',
        '  "q": "*",',
        '  "vector_query": "${1:embedding}:([${2:0.1, 0.2, ...}], k:${3:10})",',
        '  "per_page": ${4:10}',
        '}',
      ].join('\n'),
    },
    {
      label: 'Hybrid search (vector + text)',
      detail: 'template',
      body: [
        '{',
        '  "q": "$1",',
        '  "query_by": "$2",',
        '  "vector_query": "${3:embedding}:([${4:0.1, 0.2, ...}], k:${5:20}, alpha:${6:0.7})",',
        '  "per_page": ${7:10}',
        '}',
      ].join('\n'),
    },
    {
      label: 'Find similar document',
      detail: 'template',
      body: [
        '{',
        '  "q": "*",',
        '  "vector_query": "${1:embedding}:(id:${2:doc_id}, k:${3:10})",',
        '  "per_page": ${4:10}',
        '}',
      ].join('\n'),
    },
  ]

  for (const s of snippets) {
    items.push({
      label: s.label,
      kind: monaco.languages.CompletionItemKind.Snippet,
      detail: s.detail,
      insertText: s.body,
      insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
      range,
      sortText: `9_${s.label}`,
    })
  }

  return items
}

function fieldCompletions(
  monaco: typeof Monaco,
  fields: FieldInfo[],
  range: Monaco.IRange,
  prefix: string,
): Monaco.languages.CompletionItem[] {
  return fields
    .filter(f => !prefix || f.name.toLowerCase().startsWith(prefix.toLowerCase()))
    .map((f, i) => ({
      label: f.name,
      kind: monaco.languages.CompletionItemKind.Field,
      detail: f.type,
      insertText: f.name,
      range,
      sortText: String(i).padStart(3, '0'),
    }))
}

function sortByCompletions(
  monaco: typeof Monaco,
  fields: FieldInfo[],
  range: Monaco.IRange,
  prefix: string,
): Monaco.languages.CompletionItem[] {
  const items: Monaco.languages.CompletionItem[] = []
  let idx = 0

  // _text_match:desc is always useful
  if (!prefix || '_text_match'.startsWith(prefix.toLowerCase())) {
    items.push({
      label: '_text_match:desc',
      kind: monaco.languages.CompletionItemKind.Value,
      detail: 'relevance score (descending)',
      insertText: '_text_match:desc',
      range,
      sortText: String(idx++).padStart(3, '0'),
    })
  }
  // _vector_distance for vector search results
  if (!prefix || '_vector_distance'.startsWith(prefix.toLowerCase())) {
    items.push({
      label: '_vector_distance:asc',
      kind: monaco.languages.CompletionItemKind.Value,
      detail: 'vector distance (ascending, requires vector_query)',
      insertText: '_vector_distance:asc',
      range,
      sortText: String(idx++).padStart(3, '0'),
    })
  }

  for (const f of sortableFields(fields)) {
    if (prefix && !f.name.toLowerCase().startsWith(prefix.toLowerCase())) continue
    items.push({
      label: `${f.name}:asc`,
      kind: monaco.languages.CompletionItemKind.Value,
      detail: `${f.type} ascending`,
      insertText: `${f.name}:asc`,
      range,
      sortText: String(idx++).padStart(3, '0'),
    })
    items.push({
      label: `${f.name}:desc`,
      kind: monaco.languages.CompletionItemKind.Value,
      detail: `${f.type} descending`,
      insertText: `${f.name}:desc`,
      range,
      sortText: String(idx++).padStart(3, '0'),
    })
  }

  // Geo sort snippet
  for (const f of geoFields(fields)) {
    if (prefix && !f.name.toLowerCase().startsWith(prefix.toLowerCase())) continue
    items.push({
      label: `${f.name}(lat, lng):asc`,
      kind: monaco.languages.CompletionItemKind.Snippet,
      detail: 'geo distance sort',
      insertText: `${f.name}(\${1:lat}, \${2:lng}):asc`,
      insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
      range,
      sortText: String(idx++).padStart(3, '0'),
    })
  }

  return items
}

function filterByCompletions(
  monaco: typeof Monaco,
  fields: FieldInfo[],
  range: Monaco.IRange,
  prefix: string,
): Monaco.languages.CompletionItem[] {
  const items: Monaco.languages.CompletionItem[] = []
  let idx = 0

  // Logical operators
  if (!prefix || '&&'.startsWith(prefix)) {
    items.push({
      label: '&&',
      kind: monaco.languages.CompletionItemKind.Operator,
      detail: 'AND operator',
      insertText: '&& ',
      range,
      sortText: String(idx++).padStart(3, '0'),
    })
  }
  if (!prefix || '||'.startsWith(prefix)) {
    items.push({
      label: '||',
      kind: monaco.languages.CompletionItemKind.Operator,
      detail: 'OR operator',
      insertText: '|| ',
      range,
      sortText: String(idx++).padStart(3, '0'),
    })
  }

  for (const f of allIndexedFields(fields)) {
    if (prefix && !f.name.toLowerCase().startsWith(prefix.toLowerCase())) continue

    if (f.type === 'string' || f.type === 'string[]') {
      items.push({
        label: `${f.name}:=`,
        kind: monaco.languages.CompletionItemKind.Value,
        detail: `${f.type} equals`,
        insertText: `${f.name}:=\${1:value}`,
        insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
        range,
        sortText: String(idx++).padStart(3, '0'),
      })
      items.push({
        label: `${f.name}:!=`,
        kind: monaco.languages.CompletionItemKind.Value,
        detail: `${f.type} not equals`,
        insertText: `${f.name}:!=\${1:value}`,
        insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
        range,
        sortText: String(idx++).padStart(3, '0'),
      })
    } else if (f.type === 'int32' || f.type === 'int64' || f.type === 'float') {
      for (const op of [':>', ':<', ':>=', ':<=', ':=']) {
        items.push({
          label: `${f.name}${op}`,
          kind: monaco.languages.CompletionItemKind.Value,
          detail: `${f.type} ${op.slice(1)}`,
          insertText: `${f.name}${op}\${1:value}`,
          insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
          range,
          sortText: String(idx++).padStart(3, '0'),
        })
      }
      items.push({
        label: `${f.name}:[min..max]`,
        kind: monaco.languages.CompletionItemKind.Snippet,
        detail: `${f.type} range`,
        insertText: `${f.name}:[\${1:min}..\${2:max}]`,
        insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
        range,
        sortText: String(idx++).padStart(3, '0'),
      })
    } else if (f.type === 'bool') {
      items.push({
        label: `${f.name}:=true`,
        kind: monaco.languages.CompletionItemKind.Value,
        detail: 'bool equals true',
        insertText: `${f.name}:=true`,
        range,
        sortText: String(idx++).padStart(3, '0'),
      })
      items.push({
        label: `${f.name}:=false`,
        kind: monaco.languages.CompletionItemKind.Value,
        detail: 'bool equals false',
        insertText: `${f.name}:=false`,
        range,
        sortText: String(idx++).padStart(3, '0'),
      })
    } else if (f.type === 'geopoint' || f.type === 'geopoint[]') {
      items.push({
        label: `${f.name}:(lat, lng, radius)`,
        kind: monaco.languages.CompletionItemKind.Snippet,
        detail: 'geo radius filter',
        insertText: `${f.name}:(\${1:lat}, \${2:lng}, \${3:10} km)`,
        insertTextRules: monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet,
        range,
        sortText: String(idx++).padStart(3, '0'),
      })
    }
  }

  return items
}

function enumCompletions(
  monaco: typeof Monaco,
  key: string,
  range: Monaco.IRange,
): Monaco.languages.CompletionItem[] | null {
  const enums: Record<string, string[]> = {
    text_match_type: ['max_score', 'max_weight', 'sum_score'],
  }
  const values = enums[key]
  if (!values) return null
  return values.map((v, i) => ({
    label: v,
    kind: monaco.languages.CompletionItemKind.EnumMember,
    insertText: v,
    range,
    sortText: String(i).padStart(3, '0'),
  }))
}

// --- Key sets for routing value completions to the right generator ---

const fieldListKeys = new Set([
  'query_by', 'highlight_fields', 'highlight_full_fields',
  'include_fields', 'exclude_fields',
])
const facetKeys = new Set(['facet_by', 'group_by'])

// --- Main registration ---

export function registerCompletionProvider(
  monaco: typeof Monaco,
  getFields: () => FieldInfo[],
): Monaco.IDisposable {
  // JSON schema validation + hover
  monaco.json.jsonDefaults.setDiagnosticsOptions({
    validate: true,
    schemas: [
      {
        uri: 'https://torque.truespar.com/search-request.json',
        fileMatch: ['*'],
        schema: searchRequestSchema,
      },
    ],
  })

  // Context-aware completions
  const disposable = monaco.languages.registerCompletionItemProvider('json', {
    triggerCharacters: ['"', ',', ' '],

    provideCompletionItems(model, position) {
      const textBeforeCursor = model.getValueInRange({
        startLineNumber: 1,
        startColumn: 1,
        endLineNumber: position.lineNumber,
        endColumn: position.column,
      })

      const wordInfo = model.getWordUntilPosition(position)
      const range: Monaco.IRange = {
        startLineNumber: position.lineNumber,
        startColumn: wordInfo.startColumn,
        endLineNumber: position.lineNumber,
        endColumn: wordInfo.endColumn,
      }

      const ctx = detectContext(textBeforeCursor)

      if (ctx.kind === 'key') {
        return { suggestions: keyCompletions(monaco, range) }
      }

      if (ctx.kind === 'value') {
        const fields = getFields()
        const { key } = ctx

        // Extract current value text (after the last quote of "key": ")
        const valMatch = textBeforeCursor.match(/"(\w+)"\s*:\s*"([^"]*)$/)
        const rawValue = valMatch ? valMatch[2] : ''
        const segment = currentSegment(rawValue)

        // For comma-separated values, adjust range to cover only current segment
        const segmentRange: Monaco.IRange = {
          startLineNumber: position.lineNumber,
          startColumn: position.column - segment.length,
          endLineNumber: position.lineNumber,
          endColumn: position.column,
        }

        // Route to appropriate completion generator
        if (key === 'query_by') {
          return { suggestions: fieldCompletions(monaco, textFields(fields), segmentRange, segment) }
        }
        if (fieldListKeys.has(key)) {
          return { suggestions: fieldCompletions(monaco, allIndexedFields(fields), segmentRange, segment) }
        }
        if (facetKeys.has(key)) {
          return { suggestions: fieldCompletions(monaco, facetableFields(fields), segmentRange, segment) }
        }
        if (key === 'sort_by') {
          return { suggestions: sortByCompletions(monaco, fields, segmentRange, segment) }
        }
        if (key === 'filter_by') {
          return { suggestions: filterByCompletions(monaco, fields, segmentRange, segment) }
        }

        // Enum values
        const enumItems = enumCompletions(monaco, key, segmentRange)
        if (enumItems) {
          return { suggestions: enumItems }
        }
      }

      return { suggestions: [] }
    },
  })

  return disposable
}

export function getHoverContents(word: string): string | null {
  return paramDocs[word] || null
}
