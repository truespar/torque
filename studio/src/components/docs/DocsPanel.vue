<template>
  <div class="tq-panel">
    <div class="docs__scroll">
      <div class="docs__content">
        <h2 class="docs__title">Torque Documentation</h2>

        <!-- Getting Started -->
        <div class="tq-card docs__section">
          <h3>Getting Started</h3>
          <ol class="docs__list">
            <li>Start Torque: <code>torque-server --api-key YOUR_KEY</code></li>
            <li>Open Studio at <code>http://localhost:8108</code></li>
            <li>Create a collection via the Schema Browser or <code>POST /collections</code></li>
            <li>Import documents via the Import panel, HTTP API, or TCP binary ingest</li>
            <li>Query your data using the Search editor or <code>GET /collections/{name}/documents/search</code></li>
          </ol>
        </div>

        <!-- Architecture -->
        <div class="tq-card docs__section">
          <h3>Architecture</h3>
          <p>Torque is an in-memory search engine with full persistence to disk. Three interfaces:</p>
          <table class="docs__table">
            <thead><tr><th>Interface</th><th>Port</th><th>Purpose</th></tr></thead>
            <tbody>
              <tr><td><strong>HTTP API</strong></td><td>8108</td><td>Search, CRUD, Studio, admin, TLS support</td></tr>
              <tr><td><strong>TCP Binary</strong></td><td>8109</td><td>High-throughput streaming ingest (49K docs/s to searchable)</td></tr>
              <tr><td><strong>Studio</strong></td><td>8108</td><td>Embedded web UI (this page)</td></tr>
              <tr><td><strong>MCP</strong></td><td>8108</td><td>Model Context Protocol for AI agents (<code>/api/mcp</code>)</td></tr>
            </tbody>
          </table>
          <p>All data is held in memory for sub-millisecond queries. Indexes and metadata are persisted to disk and restored automatically on startup.</p>
        </div>

        <!-- Ingestion Methods -->
        <div class="tq-card docs__section">
          <h3>Ingestion Methods</h3>
          <p>Torque supports three ingestion methods, each optimized for different use cases:</p>

          <h4>1. HTTP JSONL Import (Typesense-compatible)</h4>
          <p>Standard Typesense API. Best for small updates and compatibility with existing clients.</p>
          <pre class="docs__code">POST /collections/{name}/documents/import?action=upsert
Content-Type: text/plain

{"id":"1","title":"Laptop","price":999.99}
{"id":"2","title":"Phone","price":499.99}</pre>
          <p>Actions: <code>create</code> (insert only), <code>update</code> (update only), <code>upsert</code> (insert or update).</p>

          <h4>2. TCP Binary Ingest (highest streaming throughput)</h4>
          <p>Custom binary protocol available in every Torque client SDK (.NET, Python, Node.js, Go, Java). Streams documents with zero JSON overhead. Measured throughput: 49K docs/s on a 309K-document dataset, 7.7x faster than Typesense ingestion on the same hardware (both timed until every document is searchable).</p>
          <pre class="docs__code">// .NET — stream documents to the server
await using var client = new TorqueIngestClient(options);
await client.ConnectAsync();
await client.StartIngestAsync("products");
await client.SendDocumentsAsync(documents, schema);
await client.CommitAsync();</pre>
          <p>The server builds the index asynchronously after commit. Poll <code>GET /collections/{name}</code> to check <code>num_documents</code>. See the <strong>Client SDKs</strong> section below for equivalent examples in Python, Node.js, Go, and Java.</p>

          <h4>3. Binary File Upload (TQBF format &mdash; fastest bulk import)</h4>
          <p>Pre-encode documents to a binary file, upload in one HTTP POST. Best for offline ETL, data distribution, and repeated imports — a single request imports the entire dataset with no per-document JSON parsing at the server. Every SDK ships a <code>TorqueBinaryWriter</code> that produces TQBF files.</p>
          <pre class="docs__code">// .NET — streaming binary writer, O(1) memory per document
await using var writer = new TorqueBinaryWriter(schema, "data.tqbf");
foreach (var row in db.QueryBatches("SELECT * FROM ..."))
    writer.Write(row.ToDocument());
await writer.UploadAsync(httpClient, "products", apiKey);</pre>
          <pre class="docs__code"># torque-admin CLI
torque-admin export-binary my_collection -o data.tqbf
torque-admin import-binary my_collection -f data.tqbf</pre>
          <p>TQBF v2 format: 24-byte header (<code>TQBF</code> magic, version, doc count, compression flags, uncompressed-size hint) followed by an LZ4 or ZLib compressed payload. The uncompressed-size hint lets the server pre-size its decompression buffer for multi-GB imports. All client SDKs emit ZLib (zero external dependencies); <code>torque-admin</code> can emit either LZ4 or ZLib.</p>

          <h4>Comparison</h4>
          <table class="docs__table">
            <thead><tr><th>Method</th><th>Use case</th></tr></thead>
            <tbody>
              <tr><td>HTTP JSONL</td><td>Small updates, Typesense compatibility</td></tr>
              <tr><td>TCP Binary</td><td>Live streaming from databases — 49K docs/s measured (any SDK)</td></tr>
              <tr><td>TQBF File</td><td>Bulk import, ETL, data distribution — single HTTP POST for the whole dataset (any SDK)</td></tr>
            </tbody>
          </table>
        </div>

        <!-- Client SDKs -->
        <div class="tq-card docs__section">
          <h3>Client SDKs</h3>
          <p>Torque ships with official client SDKs for five languages. Each one provides the full HTTP API, the TCP binary ingest protocol (<code>TorqueIngestClient</code>), and the TQBF binary file writer (<code>TorqueBinaryWriter</code>). All SDKs target zero external runtime dependencies.</p>
          <table class="docs__table">
            <thead><tr><th>Language</th><th>Package</th><th>Install</th></tr></thead>
            <tbody>
              <tr><td>.NET 10+</td><td><code>Torque.Http</code></td><td><code>dotnet add package Torque.Http</code></td></tr>
              <tr><td>Python 3.10+</td><td><code>torque-http</code></td><td><code>pip install torque-http</code></td></tr>
              <tr><td>Node.js 20+</td><td><code>@@truespar/torque-http</code></td><td><code>npm install @@truespar/torque-http</code></td></tr>
              <tr><td>Java 21+</td><td><code>com.truespar:torque-http</code></td><td>Maven / Gradle dependency</td></tr>
              <tr><td>Go 1.23+</td><td><code>github.com/truespar/torque/clients/go</code></td><td><code>go get</code></td></tr>
            </tbody>
          </table>
          <p>See the <a href="/torque/docs" target="_blank">Torque documentation</a> for language-specific Quick Start examples covering search, collection management, and both ingestion paths.</p>
        </div>

        <!-- Single Document CRUD -->
        <div class="tq-card docs__section">
          <h3>Document CRUD</h3>
          <table class="docs__table">
            <thead><tr><th>Operation</th><th>Method</th><th>Endpoint</th></tr></thead>
            <tbody>
              <tr><td>Create</td><td>POST</td><td><code>/collections/{name}/documents</code></td></tr>
              <tr><td>Get</td><td>GET</td><td><code>/collections/{name}/documents/{id}</code></td></tr>
              <tr><td>Update</td><td>PATCH</td><td><code>/collections/{name}/documents/{id}</code></td></tr>
              <tr><td>Delete</td><td>DELETE</td><td><code>/collections/{name}/documents/{id}</code></td></tr>
              <tr><td>Delete by filter</td><td>DELETE</td><td><code>/collections/{name}/documents?filter_by=...</code></td></tr>
              <tr><td>Export</td><td>GET</td><td><code>/collections/{name}/documents/export</code></td></tr>
            </tbody>
          </table>
        </div>

        <!-- Search API Reference -->
        <div class="tq-card docs__section">
          <h3>Search API Reference</h3>
          <p><code>GET /collections/{name}/documents/search</code></p>
          <table class="docs__table">
            <thead><tr><th>Parameter</th><th>Type</th><th>Description</th></tr></thead>
            <tbody>
              <tr><td><code>q</code></td><td>string</td><td>Search query. Use <code>*</code> for match-all.</td></tr>
              <tr><td><code>query_by</code></td><td>string</td><td>Comma-separated fields to search (required).</td></tr>
              <tr><td><code>filter_by</code></td><td>string</td><td>Filter expression: <code>field:=value</code>, <code>field:>N</code>, <code>field:[a,b]</code>. AND: <code>&&</code>, OR: <code>||</code>.</td></tr>
              <tr><td><code>sort_by</code></td><td>string</td><td>Sort fields: <code>field:asc</code> or <code>field:desc</code>. Geo: <code>field(lat,lng):asc</code>.</td></tr>
              <tr><td><code>facet_by</code></td><td>string</td><td>Comma-separated fields to facet.</td></tr>
              <tr><td><code>group_by</code></td><td>string</td><td>Group results by field value.</td></tr>
              <tr><td><code>group_limit</code></td><td>int</td><td>Max results per group (default: 3).</td></tr>
              <tr><td><code>per_page</code></td><td>int</td><td>Results per page (default: 10, max: 250).</td></tr>
              <tr><td><code>page</code></td><td>int</td><td>Page number (default: 1).</td></tr>
              <tr><td><code>num_typos</code></td><td>int</td><td>Max typos for fuzzy matching (0&ndash;2, default: 2).</td></tr>
              <tr><td><code>prefix</code></td><td>bool</td><td>Enable prefix/autocomplete matching.</td></tr>
              <tr><td><code>query_by_weights</code></td><td>string</td><td>Comma-separated weights for query_by fields.</td></tr>
              <tr><td><code>pinned_hits</code></td><td>string</td><td>Manually pin documents: <code>id1:1,id2:2</code> (id:position).</td></tr>
              <tr><td><code>hidden_hits</code></td><td>string</td><td>Hide documents from results: <code>id1,id2</code>.</td></tr>
              <tr><td><code>include_fields</code></td><td>string</td><td>Comma-separated fields to include in results.</td></tr>
              <tr><td><code>exclude_fields</code></td><td>string</td><td>Comma-separated fields to exclude from results.</td></tr>
              <tr><td><code>highlight_fields</code></td><td>string</td><td>Fields to highlight in results.</td></tr>
              <tr><td><code>synonym_sets</code></td><td>string</td><td>Comma-separated synonym set names to apply.</td></tr>
              <tr><td><code>stopwords</code></td><td>string</td><td>Comma-separated stopword set names to apply.</td></tr>
              <tr><td><code>vector_query</code></td><td>string</td><td>Vector search expression (see below).</td></tr>
              <tr><td><code>preset</code></td><td>string</td><td>Apply a saved search preset by name.</td></tr>
              <tr><td><code>text_match_type</code></td><td>string</td><td>Scoring mode: <code>max_score</code> or <code>max_weight</code>.</td></tr>
              <tr><td><code>infix</code></td><td>string</td><td>Match inside words: <code>off</code>, <code>fallback</code>, or <code>always</code> per query_by field.</td></tr>
              <tr><td><code>drop_tokens_threshold</code></td><td>int</td><td>Drop query tokens to widen results when fewer than N hits (default: 1).</td></tr>
              <tr><td><code>split_join_tokens</code></td><td>string</td><td>Try splitting/joining tokens: <code>off</code>, <code>fallback</code>, <code>always</code>.</td></tr>
              <tr><td><code>highlight_full_fields</code></td><td>string</td><td>Fields for which the full highlighted <code>value</code> is returned (snippet is always emitted).</td></tr>
              <tr><td><code>facet_return_parent</code></td><td>string</td><td>Return the parent object for nested facet fields.</td></tr>
              <tr><td><code>exhaustive_search</code></td><td>bool</td><td>Consider all candidate matches, disabling early cutoffs.</td></tr>
              <tr><td><code>search_cutoff_ms</code></td><td>int</td><td>Best-effort scoring deadline in milliseconds.</td></tr>
              <tr><td><code>use_cache</code>, <code>cache_ttl</code></td><td>bool, int</td><td>Cache the result server-side for <code>cache_ttl</code> seconds (default: 60).</td></tr>
            </tbody>
          </table>

          <h4>Vector Query</h4>
          <p>The <code>vector_query</code> parameter supports the following syntax:</p>
          <table>
            <thead><tr><th>Syntax</th><th>Description</th></tr></thead>
            <tbody>
              <tr><td><code>field:([v1,v2,...], k:10)</code></td><td>Search by vector, return k nearest neighbors.</td></tr>
              <tr><td><code>field:(id:doc_123, k:10)</code></td><td>Find similar to an existing document by its ID.</td></tr>
            </tbody>
          </table>
          <p>Optional parameters inside the parentheses:</p>
          <table>
            <thead><tr><th>Parameter</th><th>Default</th><th>Description</th></tr></thead>
            <tbody>
              <tr><td><code>k</code></td><td>100</td><td>Number of nearest neighbors (1&ndash;1000).</td></tr>
              <tr><td><code>distance_threshold</code></td><td>&mdash;</td><td>Exclude results beyond this distance.</td></tr>
              <tr><td><code>ef</code></td><td>max(k,100)</td><td>HNSW search beam width. Higher = better recall.</td></tr>
              <tr><td><code>alpha</code></td><td>&mdash;</td><td>Hybrid weight (0&ndash;1). Uses score interpolation instead of RRF.</td></tr>
              <tr><td><code>flat_search_cutoff</code></td><td>&mdash;</td><td>Brute-force if filtered set is smaller than this.</td></tr>
            </tbody>
          </table>
          <p>Sort by vector distance: <code>sort_by=_vector_distance:asc</code></p>
          <p><strong>Auto-embedding:</strong> fields with an <code>embed</code> config accept <code>vector_query=field:([], k:10)</code> — the query text is embedded server-side via the field's configured model.</p>

          <p><strong>Multi-search:</strong> <code>POST /multi_search</code> with <code>{"searches": [...]}</code> to run multiple queries in one request. Pass <code>"union": true</code> to merge all searches into a single ranked result set.</p>
        </div>

        <!-- Collections & Schema -->
        <div class="tq-card docs__section">
          <h3>Collections &amp; Schema</h3>
          <p>Collections define a schema with typed fields:</p>
          <table class="docs__table">
            <thead><tr><th>Field Type</th><th>Description</th></tr></thead>
            <tbody>
              <tr><td><code>string</code></td><td>Text field (indexed for full-text search)</td></tr>
              <tr><td><code>int32</code></td><td>32-bit integer</td></tr>
              <tr><td><code>int64</code></td><td>64-bit integer (timestamps, large IDs)</td></tr>
              <tr><td><code>float</code></td><td>64-bit float</td></tr>
              <tr><td><code>bool</code></td><td>Boolean</td></tr>
              <tr><td><code>geopoint</code></td><td>Latitude/longitude pair <code>[lat, lng]</code></td></tr>
              <tr><td><code>string[]</code></td><td>Array of strings</td></tr>
              <tr><td><code>string*</code></td><td>Scalar-or-array string (accepts either shape per document)</td></tr>
              <tr><td><code>int32[]</code>, <code>int64[]</code>, <code>float[]</code></td><td>Numeric arrays</td></tr>
              <tr><td><code>bool[]</code>, <code>geopoint[]</code></td><td>Boolean arrays (match-any filters) and multi-point geo (any-point radius, first-point sort)</td></tr>
              <tr><td><code>object</code>, <code>object[]</code>, <code>auto</code></td><td>Dynamic markers — sub-fields discovered from documents at import time (<code>object</code> types need <code>enable_nested_fields: true</code>)</td></tr>
              <tr><td><code>float[]</code> + <code>num_dim</code></td><td>Vector field for similarity search</td></tr>
            </tbody>
          </table>
          <p><strong>Field options:</strong> <code>facet</code>, <code>sort</code>, <code>index</code>, <code>optional</code>, <code>stem</code>, <code>stem_dictionary</code>, <code>locale</code>, <code>embed</code> (auto-embedding via an OpenAI-compatible endpoint).</p>
          <p><strong>Collection aliases:</strong> Use <code>PUT /aliases/{name}</code> for zero-downtime schema migrations.</p>
        </div>

        <!-- Index Modes -->
        <div class="tq-card docs__section">
          <h3>Index Modes</h3>
          <ul class="docs__list">
            <li><strong>Batch</strong> (default) &mdash; Full index rebuild via TCP or TQBF binary. Best for bulk data loads.</li>
            <li><strong>Realtime</strong> &mdash; Instant upsert/delete via HTTP or TCP Upsert. Each write is a small segment, searchable when the write returns and on disk in a write-ahead log before it is acknowledged; segments merge in the background.
              <ul class="docs__list">
                <li>Set <code>"index_mode": "realtime"</code> when creating the collection</li>
                <li>Flush to disk at 1,000 documents in memory; backpressure (429) at 10,000</li>
                <li>Manual flush: <code>POST /collections/{name}/documents/compact</code></li>
              </ul>
            </li>
          </ul>
        </div>

        <!-- Persistence -->
        <div class="tq-card docs__section">
          <h3>Persistence</h3>
          <p>All data is persisted automatically with no configuration:</p>
          <ul class="docs__list">
            <li><strong>Indexes</strong> &mdash; 13 binary component files per collection (LZ4-compressed, CRC32 checksums)</li>
            <li><strong>Write-ahead log</strong> &mdash; Every realtime write, synced before it is acknowledged (group commit)</li>
            <li><strong>Metadata</strong> &mdash; Schemas, aliases, synonyms, presets, stopwords, API keys as JSON</li>
          </ul>
          <p>Writes are atomic (temp file &rarr; fsync &rarr; rename). On startup, indexes load in parallel and each realtime collection replays its write-ahead log.</p>
        </div>

        <!-- API Management -->
        <div class="tq-card docs__section">
          <h3>API Management</h3>
          <table class="docs__table">
            <thead><tr><th>Resource</th><th>Endpoints</th></tr></thead>
            <tbody>
              <tr><td>Aliases</td><td><code>GET/PUT/DELETE /aliases/{name}</code></td></tr>
              <tr><td>Synonyms</td><td><code>GET/PUT/DELETE /synonym_sets/{name}</code> + <code>/items/{id}</code></td></tr>
              <tr><td>Presets</td><td><code>GET/PUT/DELETE /presets/{id}</code></td></tr>
              <tr><td>Stopwords</td><td><code>GET/PUT/DELETE /stopwords/{id}</code></td></tr>
              <tr><td>API Keys</td><td><code>GET/POST/DELETE /keys/{id}</code></td></tr>
              <tr><td>Stemming dictionaries</td><td><code>GET/POST /stemming/dictionaries</code> (JSONL word/root import)</td></tr>
              <tr><td>Operations</td><td><code>POST /operations/snapshot</code>, <code>/operations/db/compact</code>, <code>/operations/cache/clear</code>, <code>GET /operations/schema_changes</code>, <code>POST /config</code></td></tr>
            </tbody>
          </table>
          <p><strong>Key types:</strong> Bootstrap (full access), Admin (all actions), Search-only (queries only), Scoped (collection + filter restrictions, HMAC-signed).</p>
        </div>

        <!-- MCP Server -->
        <div class="tq-card docs__section">
          <h3>MCP Server</h3>
          <p>Torque includes a built-in MCP (Model Context Protocol) server, allowing AI assistants like Claude to search and manage your collections directly. The MCP endpoint is available at <code>POST /api/mcp</code> using JSON-RPC 2.0 (Streamable HTTP transport, protocol version 2025-11-25).</p>
          <h4>Authentication</h4>
          <p>Uses the same API key as the HTTP API: pass via <code>X-TYPESENSE-API-KEY</code> header or <code>Authorization: Bearer</code> header.</p>
          <h4>Available Tools</h4>
          <table class="docs__table">
            <thead><tr><th>Tool</th><th>Description</th></tr></thead>
            <tbody>
              <tr><td><code>search</code></td><td>Full-text search with filters, facets, sorting, typo tolerance</td></tr>
              <tr><td><code>multi_search</code></td><td>Execute multiple searches in parallel</td></tr>
              <tr><td><code>collections</code></td><td>List all collections with document counts</td></tr>
              <tr><td><code>schema</code></td><td>Get full field schema for a collection</td></tr>
              <tr><td><code>get_document</code></td><td>Retrieve a single document by ID</td></tr>
              <tr><td><code>create_collection</code></td><td>Create a new collection with typed fields</td></tr>
              <tr><td><code>upsert_document</code></td><td>Insert or replace a document</td></tr>
              <tr><td><code>delete_document</code></td><td>Delete a document by ID</td></tr>
              <tr><td><code>delete_collection</code></td><td>Delete a collection (enabled by default; disable with <code>allow_drop_collection = false</code>)</td></tr>
            </tbody>
          </table>
          <h4>Claude Desktop Configuration</h4>
          <pre class="docs__code">// claude_desktop_config.json
{
  "mcpServers": {
    "torque": {
      "url": "http://localhost:8108/api/mcp",
      "headers": {
        "X-TYPESENSE-API-KEY": "your-api-key"
      }
    }
  }
}</pre>
          <h4>Example: Initialize + Search</h4>
          <pre class="docs__code">curl -X POST http://localhost:8108/api/mcp \
  -H "X-TYPESENSE-API-KEY: your-key" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,
       "method":"initialize",
       "params":{"protocolVersion":"2025-11-25",
         "capabilities":{},
         "clientInfo":{"name":"my-app","version":"1.0"}}}'

curl -X POST http://localhost:8108/api/mcp \
  -H "X-TYPESENSE-API-KEY: your-key" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,
       "method":"tools/call",
       "params":{"name":"search",
         "arguments":{"collection":"products",
           "q":"laptop","query_by":"title"}}}'</pre>
        </div>

        <!-- Admin & Monitoring -->
        <div class="tq-card docs__section">
          <h3>Admin &amp; Monitoring</h3>
          <table class="docs__table">
            <thead><tr><th>Endpoint</th><th>Description</th></tr></thead>
            <tbody>
              <tr><td><code>GET /health</code></td><td>Health check with version</td></tr>
              <tr><td><code>GET /health/ready</code></td><td>Readiness probe (all collections loaded)</td></tr>
              <tr><td><code>GET /admin/stats</code></td><td>Memory, uptime, per-collection index size breakdown</td></tr>
              <tr><td><code>GET /metrics</code></td><td>Prometheus metrics (query latency, ingest rate, memory)</td></tr>
              <tr><td><code>GET /debug</code></td><td>Debug info (version, features, allocator)</td></tr>
            </tbody>
          </table>
          <p><strong>Slow query logging:</strong> Set <code>--slow-query-ms 100</code> to log queries exceeding the threshold at WARN level with full timing breakdown.</p>
        </div>

        <!-- TLS & Security -->
        <div class="tq-card docs__section">
          <h3>TLS &amp; Security</h3>
          <p>TLS is supported via the <code>--tls-cert</code> and <code>--tls-key</code> flags:</p>
          <pre class="docs__code">torque-server --api-key KEY --tls-cert cert.pem --tls-key key.pem</pre>
          <p>Environment variables: <code>TORQUE_TLS_CERT</code>, <code>TORQUE_TLS_KEY</code>. TOML config: <code>tls_cert</code>, <code>tls_key</code>.</p>
          <p><strong>API key auth:</strong> All API requests require the <code>X-TYPESENSE-API-KEY</code> header except <code>/health</code>, <code>/health/ready</code>, <code>/metrics</code>, and Studio routes.</p>
        </div>

        <!-- torque-admin CLI -->
        <div class="tq-card docs__section">
          <h3>torque-admin CLI</h3>
          <pre class="docs__code"># API key required (flag or TORQUE_API_KEY env var)
torque-admin --api-key KEY collections              # List collections
torque-admin --api-key KEY export my_collection     # Export schema + JSONL
torque-admin --api-key KEY import my_collection     # TCP binary import (batch mode)
torque-admin --api-key KEY import my_collection -m realtime  # Realtime upsert
torque-admin --api-key KEY delete my_collection --ids 1,2,3  # Delete by ID
torque-admin --api-key KEY export-binary my_collection -o data.tqbf  # Binary file export
torque-admin --api-key KEY import-binary my_collection -f data.tqbf  # Binary file import</pre>
        </div>

        <!-- Configuration -->
        <div class="tq-card docs__section">
          <h3>Configuration</h3>
          <p>Three-layer resolution: CLI flags &gt; environment variables (<code>TORQUE_*</code>) &gt; TOML config &gt; defaults.</p>
          <table class="docs__table">
            <thead><tr><th>Flag</th><th>Default</th><th>Description</th></tr></thead>
            <tbody>
              <tr><td><code>--api-key</code></td><td><em>auto</em></td><td>Bootstrap API key (auto-generated with <code>tq_</code> prefix if omitted)</td></tr>
              <tr><td><code>--api-addr</code></td><td><code>0.0.0.0:8108</code></td><td>HTTP API + Studio listen address</td></tr>
              <tr><td><code>--ingest-addr</code></td><td><code>0.0.0.0:8109</code></td><td>TCP binary ingest address</td></tr>
              <tr><td><code>--data-dir</code></td><td><code>./data</code></td><td>Persistence directory</td></tr>
              <tr><td><code>--log-level</code></td><td><code>info</code></td><td>trace, debug, info, warn, error</td></tr>
              <tr><td><code>--log-dir</code></td><td>&mdash;</td><td>Daily rolling log files</td></tr>
              <tr><td><code>--tls-cert</code></td><td>&mdash;</td><td>TLS certificate (PEM)</td></tr>
              <tr><td><code>--tls-key</code></td><td>&mdash;</td><td>TLS private key (PEM)</td></tr>
              <tr><td><code>--slow-query-ms</code></td><td><code>0</code></td><td>Slow query logging threshold (0 = disabled)</td></tr>
              <tr><td><code>--max-memory-percent</code></td><td><code>90</code></td><td>Max memory as % of system RAM</td></tr>
              <tr><td><code>--query-timeout-ms</code></td><td><code>10000</code></td><td>Search query timeout</td></tr>
              <tr><td><code>--max-memory-bytes</code></td><td>&mdash;</td><td>Absolute memory limit in bytes (overrides percent)</td></tr>
              <tr><td><code>--max-http-body-bytes</code></td><td><em>unlimited</em></td><td>Maximum HTTP request body size</td></tr>
              <tr><td><code>--max-ingest-connections</code></td><td><code>64</code></td><td>Maximum concurrent TCP ingest connections</td></tr>
              <tr><td><code>--max-vector-memory-gb</code></td><td><code>16</code></td><td>Maximum vector index memory in GB</td></tr>
              <tr><td><code>--gpu-vram-budget-mb</code></td><td><code>0</code></td><td>GPU VRAM budget for dense bitmaps (0 = auto 50%)</td></tr>
              <tr><td><code>--cors-origins</code></td><td><code>*</code></td><td>CORS allowed origins (comma-separated)</td></tr>
              <tr><td><code>--allowed-ips</code></td><td>&mdash;</td><td>IP allowlist (comma-separated; empty = allow all)</td></tr>
              <tr><td><code>--allow-drop-collection</code></td><td><code>true</code></td><td>Allow deleting collections via API</td></tr>
              <tr><td><code>--no-studio</code></td><td>&mdash;</td><td>Disable the embedded Studio web UI</td></tr>
              <tr><td><code>--studio-dir</code></td><td>&mdash;</td><td>Serve Studio from external directory instead of embedded</td></tr>
              <tr><td><code>--compaction-interval-secs</code></td><td><code>1800</code></td><td>Auto-compaction check interval (30 min)</td></tr>
              <tr><td><code>--compaction-memory-threshold-percent</code></td><td><code>80</code></td><td>Memory threshold to trigger compaction</td></tr>
              <tr><td><code>TORQUE_AI_PROVIDER</code></td><td>&mdash;</td><td>Search Assistant model: <code>anthropic</code> or <code>openai</code> (any OpenAI-compatible endpoint), with <code>TORQUE_AI_API_KEY</code>. Off unless set</td></tr>
              <tr><td><code>--config</code></td><td>&mdash;</td><td>Path to TOML config file (auto-detects <code>torque.toml</code>)</td></tr>
            </tbody>
          </table>
        </div>

        <!-- API Compatibility -->
        <div class="tq-card docs__section">
          <h3>API Compatibility</h3>
          <p>Torque implements the <strong>Typesense v30.1 API</strong> for search and CRUD operations &mdash; drop-in replacement for existing Typesense clients.</p>
          <p><strong>Fully compatible:</strong> Collections, Documents, Search, Multi-search (incl. union), Aliases, API Keys, Synonyms, Presets, Stopwords, Stemming dictionaries, Curation sets, Analytics, Conversational search (remote LLM), NL search, JOINs, vector + hybrid search, remote auto-embedding, operations endpoints.</p>
          <p><strong>Torque-only extensions:</strong> TCP binary ingest, TQBF binary file import, collection suspend/resume, compaction, GPU acceleration, decay sorting, infix search on any searchable field.</p>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.docs__scroll {
  height: 100%;
  overflow-y: auto;
  scrollbar-width: thin;
  scrollbar-color: var(--tv-border-default) transparent;
}

.docs__content {
  max-width: 860px;
  margin: 0 auto;
  padding: 32px 24px 64px;
}

.docs__title {
  font-size: 20px;
  font-weight: 700;
  color: var(--tv-text-primary);
  margin: 0 0 20px;
}

.docs__section {
  margin-bottom: 16px;
  padding: 16px 20px;
}

.docs__section h3 {
  font-size: 15px;
  font-weight: 700;
  color: var(--tv-text-primary);
  margin: 0 0 10px;
}

.docs__section h4 {
  font-size: 13px;
  font-weight: 600;
  color: var(--tv-accent);
  margin: 14px 0 6px;
}

.docs__list {
  padding-left: 20px;
  font-size: 13px;
  color: var(--tv-text-secondary);
  line-height: 1.8;
}

.docs__list code {
  background: var(--tv-bg-elevated);
  padding: 1px 5px;
  border-radius: 3px;
  font-size: 12px;
  font-family: var(--tv-font-mono);
}

.docs__section p {
  font-size: 13px;
  color: var(--tv-text-secondary);
  margin-bottom: 8px;
  line-height: 1.6;
}

.docs__section p code {
  background: var(--tv-bg-elevated);
  padding: 1px 5px;
  border-radius: 3px;
  font-size: 12px;
  font-family: var(--tv-font-mono);
}

.docs__code {
  background: var(--tv-bg-inset);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  padding: 10px 14px;
  font-family: var(--tv-font-mono);
  font-size: 12px;
  color: var(--tv-text-primary);
  line-height: 1.6;
  overflow-x: auto;
  margin: 8px 0;
  white-space: pre;
}

.docs__table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12px;
  margin: 8px 0;
}

.docs__table th {
  text-align: left;
  padding: 6px 10px;
  background: var(--tv-bg-elevated);
  border-bottom: 1px solid var(--tv-border-default);
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.5px;
}

.docs__table td {
  padding: 5px 10px;
  border-bottom: 1px solid var(--tv-border-subtle);
  color: var(--tv-text-secondary);
  vertical-align: top;
  line-height: 1.5;
}

.docs__table code {
  background: var(--tv-bg-elevated);
  padding: 1px 4px;
  border-radius: 3px;
  font-size: 12px;
  font-family: var(--tv-font-mono);
}
</style>
