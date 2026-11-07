<template>
  <div class="tq-panel">
    <h2>Search API Reference</h2>
    <p style="color: var(--tq-text-secondary); margin-bottom: 20px; font-size: var(--tq-font-size-sm);">
      All parameters for <code>GET /collections/{name}/documents/search</code>
    </p>

    <table class="ref-table">
      <thead><tr><th>Parameter</th><th>Type</th><th>Default</th><th>Description</th></tr></thead>
      <tbody>
        <tr v-for="p in params" :key="p.name">
          <td class="mono">{{ p.name }}</td>
          <td>{{ p.type }}</td>
          <td class="mono">{{ p.default }}</td>
          <td>{{ p.desc }}</td>
        </tr>
      </tbody>
    </table>

    <h3 style="margin-top: 32px;">Filter Syntax</h3>
    <div class="tq-card">
      <pre class="mono" style="font-size: 12px; margin: 0;">
Exact match:     field:=value
Not equal:       field:!=value
Greater than:    field:&gt;value
Less than:       field:&lt;value
Range:           field:[min..max]
Multiple values: field:=[val1, val2]
Boolean AND:     field1:=a &amp;&amp; field2:=b
Boolean OR:      field1:=a || field2:=b
Geo radius:      field:(lat, lng, radius_km km)
Nested AND:      obj:(field1:=a &amp;&amp; field2:=b)
      </pre>
    </div>

    <h3 style="margin-top: 24px;">Sort Syntax</h3>
    <div class="tq-card">
      <pre class="mono" style="font-size: 12px; margin: 0;">
Ascending:       field:asc
Descending:      field:desc
Text relevance:  _text_match:desc
Geo distance:    field(lat, lng):asc
Vector distance: _vector_distance:asc
Multi-sort:      _text_match:desc,price:asc
      </pre>
    </div>

    <h3 style="margin-top: 24px;">Vector Query Syntax</h3>
    <div class="tq-card">
      <pre class="mono" style="font-size: 12px; margin: 0;">
Basic:           field:([v1,v2,...], k:10)
Find similar:    field:(id:doc_123, k:10)
With threshold:  field:([...], k:20, distance_threshold:0.5)
Tuned recall:    field:([...], k:10, ef:200)
Hybrid alpha:    field:([...], k:20, alpha:0.7)
Flat search:     field:([...], k:10, flat_search_cutoff:500)
      </pre>
    </div>

    <h3 style="margin-top: 24px;">Realtime Operations</h3>
    <table class="ref-table">
      <thead><tr><th>Endpoint</th><th>Method</th><th>Description</th></tr></thead>
      <tbody>
        <tr v-for="op in realtimeOps" :key="op.endpoint">
          <td class="mono">{{ op.endpoint }}</td>
          <td>{{ op.method }}</td>
          <td>{{ op.desc }}</td>
        </tr>
      </tbody>
    </table>
    <p style="color: var(--tq-text-muted); font-size: var(--tq-font-size-xs); margin-top: 8px;">
      In realtime mode, upsert/delete are instant (overlay). In batch mode, single-doc changes trigger a full rebuild.
    </p>
  </div>
</template>

<script setup lang="ts">
const params = [
  { name: 'q', type: 'string', default: '', desc: 'Search query text. Use * for wildcard.' },
  { name: 'query_by', type: 'string', default: '', desc: 'Comma-separated list of fields to search against.' },
  { name: 'filter_by', type: 'string', default: '', desc: 'Filter expression to narrow results.' },
  { name: 'sort_by', type: 'string', default: '_text_match:desc', desc: 'Sort expression for ordering results.' },
  { name: 'facet_by', type: 'string', default: '', desc: 'Comma-separated fields to compute facet counts for.' },
  { name: 'max_facet_values', type: 'integer', default: '10', desc: 'Max facet values returned per field.' },
  { name: 'per_page', type: 'integer', default: '10', desc: 'Number of results per page (max 250).' },
  { name: 'page', type: 'integer', default: '1', desc: 'Page number (1-based).' },
  { name: 'prefix', type: 'boolean', default: 'true', desc: 'Enable prefix matching on query tokens.' },
  { name: 'num_typos', type: 'integer', default: '2', desc: 'Max typos per token for fuzzy matching.' },
  { name: 'query_by_weights', type: 'string', default: '', desc: 'Comma-separated weights for query_by fields.' },
  { name: 'include_fields', type: 'string', default: '', desc: 'Fields to include in results.' },
  { name: 'exclude_fields', type: 'string', default: '', desc: 'Fields to exclude from results.' },
  { name: 'highlight_fields', type: 'string', default: '', desc: 'Fields to highlight in results.' },
  { name: 'group_by', type: 'string', default: '', desc: 'Field to group results by.' },
  { name: 'group_limit', type: 'integer', default: '3', desc: 'Max hits per group.' },
  { name: 'vector_query', type: 'string', default: '', desc: 'Vector search: field:([v1,v2,...], k:N). Params: distance_threshold, ef, flat_search_cutoff, alpha, id.' },
  { name: 'preset', type: 'string', default: '', desc: 'Preset name to apply defaults from.' },
  { name: 'stopwords', type: 'string', default: '', desc: 'Stopword set ID to apply.' },
]

const realtimeOps = [
  { endpoint: '/collections/{name}/documents', method: 'POST', desc: 'Upsert single document' },
  { endpoint: '/collections/{name}/documents/{id}', method: 'PATCH', desc: 'Partial update' },
  { endpoint: '/collections/{name}/documents/{id}', method: 'DELETE', desc: 'Delete document' },
  { endpoint: '/collections/{name}/documents/import?action=upsert', method: 'POST', desc: 'Batch upsert (JSONL)' },
  { endpoint: '/collections/{name}/documents/compact', method: 'POST', desc: 'Trigger compaction' },
]
</script>

<style scoped>
.ref-table {
  width: 100%;
  border-collapse: collapse;
  font-size: var(--tq-font-size-sm);
}

.ref-table th {
  text-align: left;
  padding: 8px 12px;
  background: var(--tq-bg-elevated);
  border-bottom: 1px solid var(--tq-border);
  font-size: var(--tq-font-size-xs);
  text-transform: uppercase;
  letter-spacing: 0.5px;
  color: var(--tq-text-muted);
}

.ref-table td {
  padding: 8px 12px;
  border-bottom: 1px solid var(--tq-border-subtle);
}
</style>
