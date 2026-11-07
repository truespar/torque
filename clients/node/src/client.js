import { createHmac } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import {
  TorqueError,
  TorqueApiError,
  RequestTimeout,
  errorForStatusCode,
} from './errors.js';

// ── JSDoc Type Definitions ──────────────────────────────────────────────

/**
 * @typedef {Object} NodeConfiguration
 * @property {string} host - Hostname or IP (e.g. "localhost", "10.0.0.1").
 * @property {number} port - Port number (e.g. 8108).
 * @property {string} [protocol='http'] - "http" or "https".
 * @property {string} [path=''] - Optional URL path prefix.
 */

/**
 * @typedef {Object} ClientConfiguration
 * @property {string} [url] - Single server URL (e.g. "http://localhost:8108").
 *   Mutually exclusive with `nodes`.
 * @property {NodeConfiguration[]} [nodes] - Array of server nodes for multi-node
 *   failover. The client round-robins across healthy nodes.
 * @property {string} apiKey - API key for X-TYPESENSE-API-KEY header.
 * @property {number} [timeout=30000] - Default request timeout in milliseconds.
 * @property {number} [transferTimeout=300000] - Timeout for file uploads/downloads.
 * @property {number} [retries=3] - Number of retry attempts on transient failures.
 * @property {number} [retryDelayMs=100] - Base delay for exponential backoff (ms).
 * @property {number} [healthcheckIntervalMs=15000] - How long to skip an unhealthy
 *   node before retrying it.
 * @property {boolean} [allowUntrustedCertificate=false] - Accept self-signed TLS certs.
 *   Uses a custom undici dispatcher — does NOT set process.env.NODE_TLS_REJECT_UNAUTHORIZED.
 * @property {boolean} [sendApiKeyAsQueryParam=false] - When true, send the API key as
 *   `?x-typesense-api-key=KEY` query parameter instead of the X-TYPESENSE-API-KEY header.
 */

/**
 * Typesense v30.1 search parameters.
 *
 * @typedef {Object} SearchParameters
 * @property {string} q - Search query text. Use "*" for match-all.
 * @property {string} query_by - Comma-separated list of fields to search.
 * @property {string} [query_by_weights] - Comma-separated weights for query_by fields.
 * @property {string} [prefix] - Comma-separated booleans for prefix search per field.
 * @property {string} [infix] - Comma-separated infix mode per field ("off", "always", "fallback").
 * @property {number} [max_extra_prefix] - Max extra prefix characters for infix search.
 * @property {number} [max_extra_suffix] - Max extra suffix characters for infix search.
 * @property {string} [filter_by] - Filter expression (e.g. "price:>100 && category:=shoes").
 * @property {string} [sort_by] - Comma-separated sort fields (e.g. "price:desc,name:asc").
 * @property {string} [facet_by] - Comma-separated fields to facet on.
 * @property {number} [max_facet_values] - Max values per facet (default 10).
 * @property {string} [facet_query] - Filter facet values (e.g. "category:sho").
 * @property {string} [facet_strategy] - Facet computation strategy ("automatic", "exhaustive", "top_values").
 * @property {number} [page] - Page number (1-indexed).
 * @property {number} [per_page] - Results per page (default 10, max 250).
 * @property {number} [offset] - Offset-based pagination (alternative to page).
 * @property {number} [limit] - Limit for offset-based pagination.
 * @property {string} [group_by] - Comma-separated fields to group by.
 * @property {number} [group_limit] - Max documents per group (default 3).
 * @property {string} [include_fields] - Comma-separated fields to include in results.
 * @property {string} [exclude_fields] - Comma-separated fields to exclude.
 * @property {string} [highlight_fields] - Fields to highlight (default: query_by fields).
 * @property {string} [highlight_full_fields] - Fields to show full highlighted value.
 * @property {number} [highlight_affix_num_tokens] - Tokens around highlight match.
 * @property {string} [highlight_start_tag] - HTML tag for highlight start (default "<mark>").
 * @property {string} [highlight_end_tag] - HTML tag for highlight end (default "</mark>").
 * @property {number} [snippet_threshold] - Min character threshold for snippeting.
 * @property {number} [num_typos] - Max typos allowed (0-2, default 2).
 * @property {string} [typo_tokens_threshold] - Min results before typo tolerance kicks in.
 * @property {string} [drop_tokens_threshold] - Min results before tokens are dropped.
 * @property {number} [drop_tokens_mode] - Token drop mode.
 * @property {string} [pinned_hits] - Comma-separated "id:position" pairs.
 * @property {string} [hidden_hits] - Comma-separated document IDs to hide.
 * @property {boolean} [enable_overrides] - Enable overrides/curations (default true).
 * @property {string} [preset] - Preset name for pre-configured search params.
 * @property {boolean} [exhaustive_search] - Force exhaustive search (no WAND).
 * @property {boolean} [search_cutoff_ms] - Timeout in ms for search computation.
 * @property {boolean} [prioritize_exact_match] - Prioritize exact token matches.
 * @property {boolean} [prioritize_token_position] - Prioritize earlier token positions.
 * @property {boolean} [prioritize_num_matching_fields] - Prioritize docs matching more fields.
 * @property {string} [text_match_type] - "max_score" or "max_weight".
 * @property {number} [min_len_1typo] - Min word length for 1-typo correction.
 * @property {number} [min_len_2typo] - Min word length for 2-typo correction.
 * @property {string} [split_join_tokens] - "off", "fallback", "always".
 * @property {string} [vector_query] - Vector search query.
 * @property {string} [synonym_prefix] - Whether to apply synonyms to prefix queries.
 * @property {string} [stopwords] - Stopword set name to apply.
 * @property {string} [override_tags] - Tags for filtering overrides.
 * @property {string} [voice_query] - Voice/audio search query (base64).
 */

/**
 * A single search hit.
 *
 * @typedef {Object} Hit
 * @property {Object} document - The matched document fields.
 * @property {number} text_match - Text match score.
 * @property {number} text_match_info - Detailed text match info object.
 * @property {Object[]} [highlights] - Array of highlight objects with field, snippet, matched_tokens.
 * @property {number[]} [vector_distance] - Distance for vector search results.
 * @property {Object} [geo_distance_meters] - Geo distance for geo-sorted results.
 */

/**
 * Facet count entry.
 *
 * @typedef {Object} FacetCount
 * @property {string} field_name - Name of the faceted field.
 * @property {Object} stats - Min, max, sum, avg, total_values.
 * @property {{ value: string, count: number, highlighted: string }[]} counts - Value counts.
 * @property {{ value: string, count: number, highlighted: string }[]} [sampled_counts] - Sampled counts for top_values strategy.
 */

/**
 * Search result returned by search() and multiSearch().
 *
 * @typedef {Object} SearchResult
 * @property {number} found - Total number of documents matching the query.
 * @property {number} out_of - Total documents in collection.
 * @property {number} page - Current page number.
 * @property {number} search_time_ms - Server-side search time in milliseconds.
 * @property {Hit[]} hits - Array of matching document hits.
 * @property {FacetCount[]} [facet_counts] - Facet counts (when facet_by is used).
 * @property {Object[]} [grouped_hits] - Grouped results (when group_by is used).
 * @property {Object} [request_params] - Echo of request parameters.
 */

/**
 * Collection field definition.
 *
 * @typedef {Object} CollectionFieldSchema
 * @property {string} name - Field name (supports dotted notation for nested fields).
 * @property {string} type - Field type: "string", "int32", "int64", "float", "bool",
 *   "geopoint", "geopoint[]", "string[]", "int32[]", "int64[]", "float[]",
 *   "object", "object[]", "auto", "string*".
 * @property {boolean} [facet=false] - Enable faceting on this field.
 * @property {boolean} [optional=false] - Allow null/missing values.
 * @property {boolean} [index=true] - Include in the search index.
 * @property {boolean} [sort=false] - Enable sorting on this field.
 * @property {boolean} [infix=false] - Enable infix (substring) search.
 * @property {string} [locale] - Locale for language-specific processing.
 * @property {number} [num_dim] - Vector dimensionality (for float[] embedding fields).
 * @property {string} [vec_dist] - Vector distance metric ("cosine", "ip").
 * @property {string} [reference] - Reference to another collection for JOINs.
 * @property {string} [embed] - Embedding configuration.
 * @property {boolean} [stem=false] - Enable stemming.
 * @property {string} [drop] - Set to "true" to drop this field during schema update.
 */

/**
 * Collection schema for creation or retrieval.
 *
 * @typedef {Object} CollectionSchema
 * @property {string} name - Collection name.
 * @property {CollectionFieldSchema[]} fields - Array of field definitions.
 * @property {string} [default_sorting_field] - Default field for sorting results.
 * @property {boolean} [enable_nested_fields=false] - Enable nested object/array indexing.
 * @property {Object} [token_separators] - Custom token separator characters.
 * @property {Object} [symbols_to_index] - Symbols to index as tokens.
 */

/**
 * Parameters for exporting documents.
 *
 * @typedef {Object} ExportDocumentsParameters
 * @property {string} [filterBy] - Filter expression for documents to export.
 * @property {string} [includeFields] - Comma-separated fields to include.
 * @property {string} [excludeFields] - Comma-separated fields to exclude.
 */

// ── Helpers ─────────────────────────────────────────────────────────────

/** URL-encode a path segment. */
function _enc(s) { return encodeURIComponent(s); }

/**
 * Create a combined AbortSignal that fires when ANY of the given signals abort.
 * @param {AbortSignal[]} signals
 * @returns {AbortSignal}
 */
function _anySignal(signals) {
  const controller = new AbortController();
  for (const signal of signals) {
    if (signal.aborted) {
      controller.abort(signal.reason);
      return controller.signal;
    }
    signal.addEventListener('abort', () => controller.abort(signal.reason), { once: true });
  }
  return controller.signal;
}

/**
 * Sleep for the given number of milliseconds.
 * @param {number} ms
 * @returns {Promise<void>}
 */
function _sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

/**
 * Async generator that yields complete lines from a ReadableStream body.
 * @param {ReadableStream} body
 * @yields {string} One line at a time (without trailing newline).
 */
async function* _readLines(body) {
  const decoder = new TextDecoder();
  let buf = '';
  for await (const chunk of body) {
    buf += typeof chunk === 'string' ? chunk : decoder.decode(chunk, { stream: true });
    let nl;
    while ((nl = buf.indexOf('\n')) !== -1) {
      const line = buf.slice(0, nl);
      buf = buf.slice(nl + 1);
      if (line) yield line;
    }
  }
  // Flush remaining data (no trailing newline)
  buf += decoder.decode();
  if (buf) yield buf;
}

// ── Node Health Tracking ────────────────────────────────────────────────

/**
 * Internal tracker for multi-node health state.
 * @private
 */
class NodeState {
  /**
   * @param {NodeConfiguration} config
   */
  constructor(config) {
    /** @type {string} Resolved base URL for this node. */
    this.baseUrl = `${config.protocol ?? 'http'}://${config.host}:${config.port}${config.path ?? ''}`;
    /** @type {boolean} Whether the node is considered healthy. */
    this.healthy = true;
    /** @type {number} Timestamp when the node was marked unhealthy. */
    this.unhealthySince = 0;
  }
}

// ── TLS Agent ───────────────────────────────────────────────────────────

/**
 * Build fetch options to allow untrusted (self-signed) TLS certificates
 * WITHOUT mutating process.env.NODE_TLS_REJECT_UNAUTHORIZED.
 *
 * Node 22+ supports the `dispatcher` option in native fetch via undici.
 * We try to import undici's Agent; if unavailable (shouldn't happen on
 * Node 22 which bundles it), we fall back to setting the env var on the
 * first request only.
 *
 * @returns {Promise<Object|null>} Extra fetch options or null.
 */
let _tlsDispatcher = undefined; // cached
async function _getTlsFetchOptions() {
  if (_tlsDispatcher !== undefined) return _tlsDispatcher;
  try {
    // Node 22+ bundles undici — use its Agent to disable TLS verification
    // per-request rather than globally.
    const { Agent } = await import('undici');
    _tlsDispatcher = {
      dispatcher: new Agent({
        connect: { rejectUnauthorized: false },
      }),
    };
  } catch {
    // Fallback: set env var (global, less ideal).
    process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';
    _tlsDispatcher = null;
  }
  return _tlsDispatcher;
}

// ── TorqueHttpClient ────────────────────────────────────────────────────

/**
 * Torque search engine HTTP client. Typesense v30.1 compatible API.
 *
 * Features:
 * - Multi-node failover with round-robin and health tracking
 * - Configurable retries with exponential backoff
 * - Typed error hierarchy (see errors.js)
 * - Per-request AbortSignal support
 * - Zero external dependencies (native fetch, Node.js 22+)
 *
 * @example
 * // Single node
 * const client = new TorqueHttpClient({ url: 'http://localhost:8108', apiKey: 'your-key' });
 *
 * @example
 * // Multi-node with failover
 * const client = new TorqueHttpClient({
 *   nodes: [
 *     { host: '10.0.0.1', port: 8108, protocol: 'https' },
 *     { host: '10.0.0.2', port: 8108, protocol: 'https' },
 *     { host: '10.0.0.3', port: 8108, protocol: 'https' },
 *   ],
 *   apiKey: 'your-key',
 *   retries: 3,
 * });
 *
 * @example
 * // Backward-compatible positional form
 * const client = new TorqueHttpClient('http://localhost:8108', { apiKey: 'your-key' });
 */
export class TorqueHttpClient {
  /**
   * Create a new Torque client.
   *
   * Accepts either:
   * 1. A {@link ClientConfiguration} object (new style).
   * 2. A `(baseUrl, options)` pair (backward-compatible with v0.5).
   *
   * @param {string|ClientConfiguration} urlOrConfig - Server URL string or full config object.
   * @param {Object} [legacyOptions] - Options when using positional (url, opts) form.
   * @param {string} [legacyOptions.apiKey] - API key.
   * @param {number} [legacyOptions.timeout=30000] - Request timeout in ms.
   * @param {number} [legacyOptions.transferTimeout=300000] - Transfer timeout in ms.
   * @param {boolean} [legacyOptions.allowUntrustedCertificate=false] - Skip TLS verification.
   */
  constructor(urlOrConfig, legacyOptions) {
    /** @private @type {NodeState[]} */
    this._nodes = [];
    /** @private @type {number} Round-robin index for node selection. */
    this._nextNodeIdx = 0;

    let config;
    if (typeof urlOrConfig === 'string') {
      // Backward-compatible: TorqueHttpClient('http://...', { apiKey })
      const opts = legacyOptions ?? {};
      config = { url: urlOrConfig, ...opts };
    } else {
      config = urlOrConfig;
    }

    // Build node list
    if (config.nodes && config.nodes.length > 0) {
      for (const n of config.nodes) {
        this._nodes.push(new NodeState(n));
      }
    } else if (config.url) {
      const url = config.url.endsWith('/') ? config.url.slice(0, -1) : config.url;
      this._nodes.push(new NodeState({ host: '', port: 0, protocol: '', path: url }));
      // Override the auto-constructed URL with the raw one
      this._nodes[0].baseUrl = url;
    } else {
      throw new TorqueError('Either `url` or `nodes` must be provided');
    }

    /** @private @type {string} */
    this._apiKey = config.apiKey;
    /** @private @type {number} */
    this._timeout = config.timeout ?? 30_000;
    /** @private @type {number} */
    this._transferTimeout = config.transferTimeout ?? 300_000;
    /** @private @type {number} */
    this._retries = config.retries ?? 3;
    /** @private @type {number} */
    this._retryDelayMs = config.retryDelayMs ?? 100;
    /** @private @type {number} */
    this._healthcheckIntervalMs = config.healthcheckIntervalMs ?? 15_000;
    /** @private @type {boolean} */
    this._allowUntrustedCert = config.allowUntrustedCertificate ?? false;
    /** @private @type {boolean} */
    this._sendApiKeyAsQueryParam = config.sendApiKeyAsQueryParam ?? false;
    /** @private @type {Object|null} Cached TLS fetch options. */
    this._tlsFetchOpts = null;
    /** @private @type {boolean} */
    this._tlsInitialized = false;
  }

  // ── Health ──────────────────────────────────────────────────────────

  /**
   * Get server health status.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ ok: boolean }>}
   */
  async health(options) { return this._get('/health', options); }

  /**
   * Get server admin statistics (memory, index sizes, etc.).
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>}
   */
  async stats(options) { return this._get('/admin/stats', options); }

  /**
   * Get Prometheus metrics as text.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<string>} Prometheus exposition format text.
   */
  async metrics(options) { return this._getText('/metrics', options); }

  // ── Collections ────────────────────────────────────────────────────

  /**
   * Create a new collection.
   * @param {CollectionSchema} schema - Collection schema definition.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<CollectionSchema>} The created collection schema.
   */
  async createCollection(schema, options) { return this._post('/collections', schema, options); }

  /**
   * List all collections.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<CollectionSchema[]>} Array of collection schemas.
   */
  async listCollections(options) { return this._get('/collections', options); }

  /**
   * Get a single collection by name.
   * @param {string} name - Collection name.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<CollectionSchema>} The collection schema.
   * @throws {ObjectNotFound} If the collection does not exist.
   */
  async getCollection(name, options) { return this._get(`/collections/${_enc(name)}`, options); }

  /**
   * Update a collection schema (add/drop fields, change settings).
   * @param {string} name - Collection name.
   * @param {Object} schema - Partial schema update. Use `{ fields: [{ name: "x", drop: true }] }` to drop fields.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<CollectionSchema>} The updated collection schema.
   * @throws {ObjectNotFound} If the collection does not exist.
   */
  async updateCollection(name, schema, options) {
    return this._patch(`/collections/${_enc(name)}`, schema, options);
  }

  /**
   * Delete a collection and all its documents.
   * @param {string} name - Collection name.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<void>}
   * @throws {ObjectNotFound} If the collection does not exist.
   */
  async deleteCollection(name, options) { await this._delete(`/collections/${_enc(name)}`, options); }

  /**
   * Delete all documents in a collection while keeping the schema intact.
   * Equivalent to `DELETE /collections/{name}/documents` with no filter.
   * @param {string} name - Collection name.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ num_deleted: number }>} Number of deleted documents.
   * @throws {ObjectNotFound} If the collection does not exist.
   */
  async truncateCollection(name, options) {
    return this._deleteJson(`/collections/${_enc(name)}/documents`, options);
  }

  // ── Documents ──────────────────────────────────────────────────────

  /**
   * Create a single document in a collection.
   * @param {string} collection - Collection name.
   * @param {Object} document - Document object. Must contain an "id" field.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @param {string} [options.dirtyValues] - How to handle dirty values: "coerce_or_reject",
   *   "coerce_or_drop", "drop", "reject" (default server behavior: "coerce_or_reject").
   * @returns {Promise<Object>} The created document.
   * @throws {ObjectAlreadyExists} If a document with the same ID exists.
   */
  async createDocument(collection, document, options) {
    let path = `/collections/${_enc(collection)}/documents`;
    if (options?.dirtyValues) path += `?dirty_values=${_enc(options.dirtyValues)}`;
    return this._post(path, document, options);
  }

  /**
   * Upsert a single document (create or replace).
   * @param {string} collection - Collection name.
   * @param {Object} document - Document object. Must contain an "id" field.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @param {string} [options.dirtyValues] - How to handle dirty values: "coerce_or_reject",
   *   "coerce_or_drop", "drop", "reject".
   * @returns {Promise<Object>} The upserted document.
   */
  async upsertDocument(collection, document, options) {
    let path = `/collections/${_enc(collection)}/documents?action=upsert`;
    if (options?.dirtyValues) path += `&dirty_values=${_enc(options.dirtyValues)}`;
    return this._post(path, document, options);
  }

  /**
   * Get a document by its ID.
   * @param {string} collection - Collection name.
   * @param {string} documentId - Document ID.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>} The document.
   * @throws {ObjectNotFound} If the document does not exist.
   */
  async getDocument(collection, documentId, options) {
    return this._get(`/collections/${_enc(collection)}/documents/${_enc(documentId)}`, options);
  }

  /**
   * Partially update a document by its ID.
   * @param {string} collection - Collection name.
   * @param {string} documentId - Document ID.
   * @param {Object} fields - Fields to update.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>} The updated document.
   * @throws {ObjectNotFound} If the document does not exist.
   */
  async updateDocument(collection, documentId, fields, options) {
    return this._patch(`/collections/${_enc(collection)}/documents/${_enc(documentId)}`, fields, options);
  }

  /**
   * Delete a document by its ID.
   * @param {string} collection - Collection name.
   * @param {string} documentId - Document ID.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<void>}
   * @throws {ObjectNotFound} If the document does not exist.
   */
  async deleteDocument(collection, documentId, options) {
    await this._delete(`/collections/${_enc(collection)}/documents/${_enc(documentId)}`, options);
  }

  /**
   * Delete documents matching a filter expression.
   * @param {string} collection - Collection name.
   * @param {string} filterBy - Filter expression (e.g. "status:=inactive").
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ num_deleted: number }>} Number of deleted documents.
   */
  async deleteByFilter(collection, filterBy, options) {
    return this._deleteJson(
      `/collections/${_enc(collection)}/documents?filter_by=${encodeURIComponent(filterBy)}`,
      options,
    );
  }

  /**
   * Delete documents matching a filter expression.
   * Alias for {@link deleteByFilter} for Typesense SDK compatibility.
   * @param {string} collection - Collection name.
   * @param {string} filterBy - Filter expression.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ num_deleted: number }>}
   */
  async deleteDocumentsByFilter(collection, filterBy, options) {
    return this.deleteByFilter(collection, filterBy, options);
  }

  /**
   * Update documents matching a filter expression.
   * @param {string} collection - Collection name.
   * @param {Object} document - Fields to set on matching documents.
   * @param {string} filterBy - Filter expression (e.g. "status:=pending").
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ num_updated: number }>} Number of updated documents.
   */
  async updateDocumentsByFilter(collection, document, filterBy, options) {
    return this._patch(
      `/collections/${_enc(collection)}/documents?filter_by=${encodeURIComponent(filterBy)}`,
      document,
      options,
    );
  }

  /**
   * Import documents via JSONL.
   * @param {string} collection - Collection name.
   * @param {Object[]} documents - Array of document objects.
   * @param {string} [action='upsert'] - Import action: "create", "upsert", "update", "emplace".
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @param {string} [options.dirtyValues] - How to handle dirty values: "coerce_or_reject",
   *   "coerce_or_drop", "drop", "reject".
   * @param {number} [options.batchSize] - When set, split documents into chunks of this size
   *   and send as separate requests. Results from all chunks are concatenated.
   * @param {boolean} [options.returnDoc] - When true, include the document in each result (`return_doc=true`).
   * @param {boolean} [options.returnId] - When true, include the document ID in each result (`return_id=true`).
   * @returns {Promise<Object[]>} Per-document import results with `{ success: boolean }`.
   */
  async importDocuments(collection, documents, action = 'upsert', options) {
    let path = `/collections/${_enc(collection)}/documents/import?action=${_enc(action)}`;
    if (options?.dirtyValues) path += `&dirty_values=${_enc(options.dirtyValues)}`;
    if (options?.returnDoc) path += `&return_doc=true`;
    if (options?.returnId) path += `&return_id=true`;

    const batchSize = options?.batchSize;
    if (batchSize && batchSize > 0 && documents.length > batchSize) {
      const results = [];
      for (let i = 0; i < documents.length; i += batchSize) {
        const chunk = documents.slice(i, i + batchSize);
        const body = chunk.map(d => JSON.stringify(d)).join('\n');
        const text = await this._postText(path, body, 'text/plain', options);
        for (const l of text.trim().split('\n').filter(Boolean)) results.push(JSON.parse(l));
      }
      return results;
    }

    const body = documents.map(d => JSON.stringify(d)).join('\n');
    const text = await this._postText(path, body, 'text/plain', options);
    return text.trim().split('\n').filter(Boolean).map(l => JSON.parse(l));
  }

  /**
   * Import a raw JSONL string.
   * @param {string} collection - Collection name.
   * @param {string} jsonl - Newline-delimited JSON string.
   * @param {string} [action='upsert'] - Import action.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @param {string} [options.dirtyValues] - How to handle dirty values.
   * @param {number} [options.batchSize] - When set, split lines into chunks of this size
   *   and send as separate requests. Results from all chunks are concatenated.
   * @param {boolean} [options.returnDoc] - When true, include the document in each result (`return_doc=true`).
   * @param {boolean} [options.returnId] - When true, include the document ID in each result (`return_id=true`).
   * @returns {Promise<Object[]>} Per-document import results.
   */
  async importJsonl(collection, jsonl, action = 'upsert', options) {
    let path = `/collections/${_enc(collection)}/documents/import?action=${_enc(action)}`;
    if (options?.dirtyValues) path += `&dirty_values=${_enc(options.dirtyValues)}`;
    if (options?.returnDoc) path += `&return_doc=true`;
    if (options?.returnId) path += `&return_id=true`;

    const batchSize = options?.batchSize;
    if (batchSize && batchSize > 0) {
      const lines = jsonl.trim().split('\n').filter(Boolean);
      if (lines.length > batchSize) {
        const results = [];
        for (let i = 0; i < lines.length; i += batchSize) {
          const chunk = lines.slice(i, i + batchSize).join('\n');
          const text = await this._postText(path, chunk, 'text/plain', options);
          for (const l of text.trim().split('\n').filter(Boolean)) results.push(JSON.parse(l));
        }
        return results;
      }
    }

    const text = await this._postText(path, jsonl, 'text/plain', options);
    return text.trim().split('\n').filter(Boolean).map(l => JSON.parse(l));
  }

  /**
   * Export documents as JSONL text.
   * @param {string} collection - Collection name.
   * @param {ExportDocumentsParameters} [params] - Optional export parameters.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<string>} JSONL text with one document per line.
   */
  async exportDocuments(collection, params, options) {
    const qsParts = [];
    if (params?.filterBy) qsParts.push(`filter_by=${encodeURIComponent(params.filterBy)}`);
    if (params?.includeFields) qsParts.push(`include_fields=${encodeURIComponent(params.includeFields)}`);
    if (params?.excludeFields) qsParts.push(`exclude_fields=${encodeURIComponent(params.excludeFields)}`);
    const qs = qsParts.length > 0 ? `?${qsParts.join('&')}` : '';
    return this._getText(`/collections/${_enc(collection)}/documents/export${qs}`, options);
  }

  /**
   * Upload a TQBF binary file for high-speed import.
   * @param {string} collection - Collection name.
   * @param {string} filePath - Path to the .tqbf file.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>} Import result.
   */
  async importBinary(collection, filePath, options) {
    const data = await readFile(filePath);
    return this._postBytes(`/collections/${_enc(collection)}/documents/import-binary`, data, options);
  }

  // ── Search ─────────────────────────────────────────────────────────

  /**
   * Search documents in a collection.
   * @param {string} collection - Collection name.
   * @param {SearchParameters} [params={}] - Typesense v30.1 search parameters.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<SearchResult>} Search result with found, hits, facet_counts.
   */
  async search(collection, params = {}, options) {
    const qs = new URLSearchParams(
      Object.fromEntries(
        Object.entries(params)
          .filter(([, v]) => v != null)
          .map(([k, v]) => [k, String(v)]),
      ),
    ).toString();
    return this._get(`/collections/${_enc(collection)}/documents/search?${qs}`, options);
  }

  /**
   * Execute multiple searches in a single HTTP request.
   * @param {{ collection: string, q: string, query_by: string, [key: string]: any }[]} searches - Array of search request objects.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @param {Object} [options.commonParams] - Common search parameters applied to all searches
   *   (e.g. { query_by: "title", per_page: 5 }). Sent as query params on the URL.
   * @returns {Promise<{ results: SearchResult[] }>} Array of search results.
   */
  async multiSearch(searches, { commonParams, ...options } = {}) {
    let path = '/multi_search';
    if (commonParams) {
      const qs = new URLSearchParams(
        Object.fromEntries(
          Object.entries(commonParams)
            .filter(([, v]) => v != null)
            .map(([k, v]) => [k, String(v)]),
        ),
      ).toString();
      if (qs) path += `?${qs}`;
    }
    return this._post(path, { searches }, options);
  }

  /**
   * Execute multiple searches merged into a single ranked result set
   * (Typesense v30.1 `union: true`). Hits from all searches are merged and
   * ranked by text_match. Not compatible with group_by searches.
   * @param {{ collection: string, q: string, query_by: string, [key: string]: any }[]} searches - Array of search request objects.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @param {Object} [options.commonParams] - Common search parameters applied to all searches.
   *   `page`/`per_page` here paginate the merged result set.
   * @returns {Promise<SearchResult>} A single merged search result.
   */
  async multiSearchUnion(searches, { commonParams, ...options } = {}) {
    let path = '/multi_search';
    if (commonParams) {
      const qs = new URLSearchParams(
        Object.fromEntries(
          Object.entries(commonParams)
            .filter(([, v]) => v != null)
            .map(([k, v]) => [k, String(v)]),
        ),
      ).toString();
      if (qs) path += `?${qs}`;
    }
    return this._post(path, { searches, union: true }, options);
  }

  // ── Aliases ────────────────────────────────────────────────────────

  /**
   * Create or update a collection alias.
   * @param {string} name - Alias name.
   * @param {string} collectionName - Target collection name.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ name: string, collection_name: string }>}
   */
  async upsertAlias(name, collectionName, options) {
    return this._put(`/aliases/${_enc(name)}`, { collection_name: collectionName }, options);
  }

  /**
   * Get an alias by name.
   * @param {string} name - Alias name.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ name: string, collection_name: string }>}
   * @throws {ObjectNotFound} If the alias does not exist.
   */
  async getAlias(name, options) { return this._get(`/aliases/${_enc(name)}`, options); }

  /**
   * List all aliases.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ aliases: { name: string, collection_name: string }[] }>}
   */
  async listAliases(options) { return this._get('/aliases', options); }

  /**
   * Delete an alias.
   * @param {string} name - Alias name.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<void>}
   */
  async deleteAlias(name, options) { await this._delete(`/aliases/${_enc(name)}`, options); }

  // ── API Keys ───────────────────────────────────────────────────────

  /**
   * Create an API key.
   * @param {{ description: string, actions: string[], collections: string[], value?: string, expires_at?: number }} keySpec - Key specification.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ id: number, value: string, description: string, actions: string[], collections: string[] }>}
   */
  async createKey(keySpec, options) { return this._post('/keys', keySpec, options); }

  /**
   * List all API keys (values are redacted).
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ keys: Object[] }>}
   */
  async listKeys(options) { return this._get('/keys', options); }

  /**
   * Get an API key by ID.
   * @param {number} id - Key ID.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>} Key metadata (value is redacted).
   * @throws {ObjectNotFound} If the key does not exist.
   */
  async getKey(id, options) { return this._get(`/keys/${id}`, options); }

  /**
   * Delete an API key.
   * @param {number} id - Key ID.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<void>}
   */
  async deleteKey(id, options) { await this._delete(`/keys/${id}`, options); }

  /**
   * Generate a scoped search API key using HMAC-SHA256.
   *
   * Scoped keys embed search parameters (filter_by, expires_at, etc.) into the key
   * itself, allowing client-side search with server-enforced restrictions. The key
   * is generated entirely client-side with no server round-trip.
   *
   * @param {string} searchKey - The parent search-only API key to derive from.
   * @param {Object} params - Embedded search parameters.
   * @param {string} [params.filter_by] - Enforced filter (cannot be overridden by the searcher).
   * @param {string} [params.collection] - Restrict to a single collection.
   * @param {number} [params.expires_at] - Unix timestamp when this scoped key expires.
   * @param {string} [params.cache_ttl] - Cache TTL for search results.
   * @param {string} [params.limit_multi_searches] - Max number of searches in multi_search.
   * @returns {string} Base64-encoded scoped search key.
   */
  generateScopedSearchKey(searchKey, params) {
    const paramsStr = JSON.stringify(params);
    const digest = createHmac('sha256', searchKey)
      .update(paramsStr)
      .digest('base64');
    // Typesense format: base64(hmac) + base64(params)
    const paramsB64 = Buffer.from(paramsStr).toString('base64');
    return `${digest}${paramsB64}`;
  }

  // ── Synonyms ───────────────────────────────────────────────────────

  /**
   * Create or update a synonym set.
   * @param {string} name - Synonym set name.
   * @param {{ synonyms?: string[], root?: string, symbols_to_index?: string[] }} body - Synonym definition.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>}
   */
  async upsertSynonymSet(name, body, options) { return this._put(`/synonym_sets/${_enc(name)}`, body, options); }

  /**
   * Get a single synonym set by name within a collection.
   * @param {string} collection - Collection name.
   * @param {string} name - Synonym set name.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>} The synonym set.
   * @throws {ObjectNotFound} If the synonym set does not exist.
   */
  async getSynonymSet(collection, name, options) {
    return this._get(`/collections/${_enc(collection)}/synonym_sets/${_enc(name)}`, options);
  }

  /**
   * List all synonym sets.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ synonym_sets: Object[] }>}
   */
  async listSynonymSets(options) { return this._get('/synonym_sets', options); }

  /**
   * Delete a synonym set.
   * @param {string} name - Synonym set name.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<void>}
   */
  async deleteSynonymSet(name, options) { await this._delete(`/synonym_sets/${_enc(name)}`, options); }

  // ── Presets ────────────────────────────────────────────────────────

  /**
   * Create or update a search preset.
   * @param {string} id - Preset ID.
   * @param {{ value: SearchParameters }} body - Preset value containing search params.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>}
   */
  async upsertPreset(id, body, options) { return this._put(`/presets/${_enc(id)}`, body, options); }

  /**
   * Get a single preset by ID.
   * @param {string} name - Preset name/ID.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>} The preset.
   * @throws {ObjectNotFound} If the preset does not exist.
   */
  async getPreset(name, options) { return this._get(`/presets/${_enc(name)}`, options); }

  /**
   * List all presets.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ presets: Object[] }>}
   */
  async listPresets(options) { return this._get('/presets', options); }

  /**
   * Delete a preset.
   * @param {string} id - Preset ID.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<void>}
   */
  async deletePreset(id, options) { await this._delete(`/presets/${_enc(id)}`, options); }

  // ── Stopwords ──────────────────────────────────────────────────────

  /**
   * Create or update a stopword set.
   * @param {string} id - Stopword set ID.
   * @param {{ stopwords: string[], locale?: string }} body - Stopword definition.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>}
   */
  async upsertStopwords(id, body, options) { return this._put(`/stopwords/${_enc(id)}`, body, options); }

  /**
   * Get a single stopword set by ID.
   * @param {string} name - Stopword set ID.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object>} The stopword set.
   * @throws {ObjectNotFound} If the stopword set does not exist.
   */
  async getStopwordsSet(name, options) { return this._get(`/stopwords/${_enc(name)}`, options); }

  /**
   * List all stopword sets.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<{ stopwords: Object[] }>}
   */
  async listStopwords(options) { return this._get('/stopwords', options); }

  /**
   * Delete a stopword set.
   * @param {string} id - Stopword set ID.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<void>}
   */
  async deleteStopwords(id, options) { await this._delete(`/stopwords/${_enc(id)}`, options); }

  // ── Streaming ──────────────────────────────────────────────────────

  /**
   * Export documents as a streaming async iterator of JSONL lines.
   *
   * Each yielded value is a single JSON string (one document). Parse with
   * `JSON.parse()` as needed. This avoids buffering the entire export in memory.
   *
   * @param {string} collection - Collection name.
   * @param {ExportDocumentsParameters} [params] - Optional export parameters.
   * @param {Object} [options] - Request options.
   * @param {AbortSignal} [options.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {AsyncGenerator<string>} Async iterator yielding one JSONL line per document.
   */
  async *exportDocumentsStream(collection, params, options) {
    const qsParts = [];
    if (params?.filterBy) qsParts.push(`filter_by=${encodeURIComponent(params.filterBy)}`);
    if (params?.includeFields) qsParts.push(`include_fields=${encodeURIComponent(params.includeFields)}`);
    if (params?.excludeFields) qsParts.push(`exclude_fields=${encodeURIComponent(params.excludeFields)}`);
    const qs = qsParts.length > 0 ? `?${qsParts.join('&')}` : '';
    const path = this._applyApiKey(`/collections/${_enc(collection)}/documents/export${qs}`);

    const resp = await this._doStreamRequest('GET', path, {
      headers: this._headers(),
    }, this._transferTimeout, options?.abortSignal);

    yield* _readLines(resp.body);
  }

  /**
   * Import documents by streaming a ReadableStream of JSONL data to the server.
   *
   * The stream should emit chunks of UTF-8 text (string or Uint8Array) containing
   * newline-delimited JSON documents. This avoids buffering the entire payload in memory.
   *
   * @param {string} collection - Collection name.
   * @param {ReadableStream|AsyncIterable} readableStream - Stream of JSONL data.
   * @param {Object} [opts] - Import options.
   * @param {string} [opts.action='upsert'] - Import action: "create", "upsert", "update", "emplace".
   * @param {string} [opts.dirtyValues] - How to handle dirty values.
   * @param {AbortSignal} [opts.abortSignal] - Optional AbortSignal for cancellation.
   * @returns {Promise<Object[]>} Per-document import results.
   */
  async importDocumentsStream(collection, readableStream, opts) {
    const action = opts?.action ?? 'upsert';
    let path = `/collections/${_enc(collection)}/documents/import?action=${_enc(action)}`;
    if (opts?.dirtyValues) path += `&dirty_values=${_enc(opts.dirtyValues)}`;

    // Convert async iterables to a ReadableStream for fetch body
    let body = readableStream;
    if (readableStream[Symbol.asyncIterator] && !readableStream.getReader) {
      body = ReadableStream.from(readableStream);
    }

    // Stream bodies cannot be replayed on retry, so use _doStreamRequest
    // (single attempt per node) and read the response text ourselves.
    const resp = await this._doStreamRequest('POST', this._applyApiKey(path), {
      headers: this._headers({ 'Content-Type': 'text/plain' }),
      body,
      duplex: 'half',
    }, this._transferTimeout, opts?.abortSignal);

    const text = await resp.text();
    return text.trim().split('\n').filter(Boolean).map(l => JSON.parse(l));
  }

  // ── Node Selection ────────────────────────────────────────────────

  /**
   * Select the next healthy node using round-robin.
   * Unhealthy nodes are retried after healthcheckIntervalMs has elapsed.
   *
   * @private
   * @returns {NodeState} A node to try.
   * @throws {TorqueError} If all nodes are unhealthy and none are due for healthcheck.
   */
  _selectNode() {
    const now = Date.now();
    const len = this._nodes.length;

    // Try up to len nodes starting from the current round-robin index
    for (let i = 0; i < len; i++) {
      const idx = (this._nextNodeIdx + i) % len;
      const node = this._nodes[idx];
      if (node.healthy) {
        this._nextNodeIdx = (idx + 1) % len;
        return node;
      }
      // Check if enough time has passed to retry this unhealthy node
      if (now - node.unhealthySince >= this._healthcheckIntervalMs) {
        this._nextNodeIdx = (idx + 1) % len;
        return node;
      }
    }

    // All nodes are unhealthy and not yet due for retry — pick the one
    // that has been unhealthy the longest (most likely to have recovered).
    let oldest = this._nodes[0];
    for (let i = 1; i < len; i++) {
      if (this._nodes[i].unhealthySince < oldest.unhealthySince) {
        oldest = this._nodes[i];
      }
    }
    return oldest;
  }

  /**
   * Mark a node as unhealthy after a request failure.
   * @private
   * @param {NodeState} node
   */
  _markUnhealthy(node) {
    node.healthy = false;
    node.unhealthySince = Date.now();
  }

  /**
   * Mark a node as healthy after a successful request.
   * @private
   * @param {NodeState} node
   */
  _markHealthy(node) {
    node.healthy = true;
    node.unhealthySince = 0;
  }

  // ── Internal HTTP (with retry + failover) ─────────────────────────

  /**
   * Build the default headers for all requests.
   * When sendApiKeyAsQueryParam is true, the API key is omitted from headers
   * and must be appended to the URL via _applyApiKey().
   * @private
   * @param {Object} [extra] - Additional headers to merge.
   * @returns {Object} Headers object.
   */
  _headers(extra = {}) {
    if (this._sendApiKeyAsQueryParam) return { ...extra };
    return { 'X-TYPESENSE-API-KEY': this._apiKey, ...extra };
  }

  /**
   * Append the API key as a query parameter when sendApiKeyAsQueryParam is enabled.
   * @private
   * @param {string} path - URL path, possibly with existing query string.
   * @returns {string} Path with the API key appended when needed.
   */
  _applyApiKey(path) {
    if (!this._sendApiKeyAsQueryParam) return path;
    const sep = path.includes('?') ? '&' : '?';
    return `${path}${sep}x-typesense-api-key=${encodeURIComponent(this._apiKey)}`;
  }

  /**
   * Lazily initialize TLS options if needed.
   * @private
   * @returns {Promise<Object|null>}
   */
  async _getTlsOpts() {
    if (!this._allowUntrustedCert) return null;
    if (!this._tlsInitialized) {
      this._tlsFetchOpts = await _getTlsFetchOptions();
      this._tlsInitialized = true;
    }
    return this._tlsFetchOpts;
  }

  /**
   * Build the AbortSignal for a request, combining a timeout with an optional
   * user-supplied signal.
   *
   * @private
   * @param {number} timeoutMs - Timeout in milliseconds.
   * @param {AbortSignal} [userSignal] - Optional user-supplied AbortSignal.
   * @returns {AbortSignal}
   */
  _buildSignal(timeoutMs, userSignal) {
    const timeoutSignal = AbortSignal.timeout(timeoutMs);
    if (!userSignal) return timeoutSignal;
    return _anySignal([timeoutSignal, userSignal]);
  }

  /**
   * Core request method with retry logic and multi-node failover.
   *
   * On failure:
   * - Timeouts, network errors, and 503s trigger a retry on the next node.
   * - Other 4xx/5xx errors are thrown immediately (no retry).
   *
   * @private
   * @param {string} method - HTTP method.
   * @param {string} path - URL path (appended to node base URL).
   * @param {Object} fetchOpts - Options for fetch (headers, body, etc.).
   * @param {number} timeoutMs - Timeout for this request.
   * @param {'json'|'text'|'none'} responseMode - How to parse the response body.
   * @param {AbortSignal} [userSignal] - Optional user-supplied AbortSignal.
   * @returns {Promise<any>} Parsed response.
   */
  async _doRequest(method, path, fetchOpts, timeoutMs, responseMode, userSignal) {
    const tlsOpts = await this._getTlsOpts();
    let lastError;

    for (let attempt = 0; attempt <= this._retries; attempt++) {
      const node = this._selectNode();

      try {
        const signal = this._buildSignal(timeoutMs, userSignal);
        const resp = await fetch(`${node.baseUrl}${path}`, {
          method,
          ...fetchOpts,
          signal,
          ...(tlsOpts ?? {}),
        });

        // 503 is retryable
        if (resp.status === 503) {
          let msg = `HTTP 503`;
          try { const d = await resp.json(); msg = d.message ?? msg; } catch {}
          this._markUnhealthy(node);
          lastError = errorForStatusCode(503, msg);
          if (attempt < this._retries) {
            await _sleep(this._retryDelayMs * 2 ** attempt);
            continue;
          }
          throw lastError;
        }

        // Any other non-2xx — throw immediately, no retry
        if (resp.status < 200 || resp.status >= 300) {
          let msg = `HTTP ${resp.status}`;
          try { const d = await resp.json(); msg = d.message ?? d.error ?? msg; } catch {}
          throw errorForStatusCode(resp.status, msg);
        }

        // Success — mark node healthy and return
        this._markHealthy(node);

        switch (responseMode) {
          case 'json': {
            const text = await resp.text();
            return text ? JSON.parse(text) : null;
          }
          case 'text':
            return resp.text();
          case 'none':
            await resp.text(); // drain body
            return undefined;
          default:
            return undefined;
        }
      } catch (err) {
        // If the error is one of our typed API errors (non-retryable), rethrow
        if (err instanceof TorqueApiError && err.statusCode !== 503) {
          throw err;
        }

        // Timeout or network error — retryable
        this._markUnhealthy(node);

        if (err.name === 'TimeoutError' || err.name === 'AbortError') {
          lastError = new RequestTimeout(
            `Request to ${node.baseUrl}${path} timed out after ${timeoutMs}ms`,
          );
        } else if (err instanceof TorqueApiError) {
          lastError = err;
        } else {
          lastError = new TorqueError(err.message);
        }

        if (attempt < this._retries) {
          await _sleep(this._retryDelayMs * 2 ** attempt);
          continue;
        }
      }
    }

    throw lastError;
  }

  /**
   * @private
   * @param {string} path
   * @param {Object} [options]
   * @returns {Promise<any>}
   */
  async _get(path, options) {
    return this._doRequest('GET', this._applyApiKey(path), {
      headers: this._headers(),
    }, this._timeout, 'json', options?.abortSignal);
  }

  /**
   * @private
   * @param {string} path
   * @param {Object} [options]
   * @returns {Promise<string>}
   */
  async _getText(path, options) {
    return this._doRequest('GET', this._applyApiKey(path), {
      headers: this._headers(),
    }, this._timeout, 'text', options?.abortSignal);
  }

  /**
   * @private
   * @param {string} path
   * @param {Object} body
   * @param {Object} [options]
   * @returns {Promise<any>}
   */
  async _post(path, body, options) {
    return this._doRequest('POST', this._applyApiKey(path), {
      headers: this._headers({ 'Content-Type': 'application/json' }),
      body: JSON.stringify(body),
    }, this._timeout, 'json', options?.abortSignal);
  }

  /**
   * @private
   * @param {string} path
   * @param {string} body
   * @param {string} contentType
   * @param {Object} [options]
   * @returns {Promise<string>}
   */
  async _postText(path, body, contentType, options) {
    return this._doRequest('POST', this._applyApiKey(path), {
      headers: this._headers({ 'Content-Type': contentType }),
      body,
    }, this._transferTimeout, 'text', options?.abortSignal);
  }

  /**
   * @private
   * @param {string} path
   * @param {Buffer} body
   * @param {Object} [options]
   * @returns {Promise<any>}
   */
  async _postBytes(path, body, options) {
    return this._doRequest('POST', this._applyApiKey(path), {
      headers: this._headers({ 'Content-Type': 'application/octet-stream' }),
      body,
    }, this._transferTimeout, 'json', options?.abortSignal);
  }

  /**
   * @private
   * @param {string} path
   * @param {Object} body
   * @param {Object} [options]
   * @returns {Promise<any>}
   */
  async _put(path, body, options) {
    return this._doRequest('PUT', this._applyApiKey(path), {
      headers: this._headers({ 'Content-Type': 'application/json' }),
      body: JSON.stringify(body),
    }, this._timeout, 'json', options?.abortSignal);
  }

  /**
   * @private
   * @param {string} path
   * @param {Object} body
   * @param {Object} [options]
   * @returns {Promise<any>}
   */
  async _patch(path, body, options) {
    return this._doRequest('PATCH', this._applyApiKey(path), {
      headers: this._headers({ 'Content-Type': 'application/json' }),
      body: JSON.stringify(body),
    }, this._timeout, 'json', options?.abortSignal);
  }

  /**
   * @private
   * @param {string} path
   * @param {Object} [options]
   * @returns {Promise<void>}
   */
  async _delete(path, options) {
    return this._doRequest('DELETE', this._applyApiKey(path), {
      headers: this._headers(),
    }, this._timeout, 'none', options?.abortSignal);
  }

  /**
   * @private
   * @param {string} path
   * @param {Object} [options]
   * @returns {Promise<any>}
   */
  async _deleteJson(path, options) {
    return this._doRequest('DELETE', this._applyApiKey(path), {
      headers: this._headers(),
    }, this._timeout, 'json', options?.abortSignal);
  }

  /**
   * Like _doRequest, but returns the raw Response on success (for streaming).
   * Retry and failover logic is identical.
   *
   * @private
   * @param {string} method
   * @param {string} path
   * @param {Object} fetchOpts
   * @param {number} timeoutMs
   * @param {AbortSignal} [userSignal]
   * @returns {Promise<Response>}
   */
  async _doStreamRequest(method, path, fetchOpts, timeoutMs, userSignal) {
    const tlsOpts = await this._getTlsOpts();
    let lastError;

    for (let attempt = 0; attempt <= this._retries; attempt++) {
      const node = this._selectNode();

      try {
        const signal = this._buildSignal(timeoutMs, userSignal);
        const resp = await fetch(`${node.baseUrl}${path}`, {
          method,
          ...fetchOpts,
          signal,
          ...(tlsOpts ?? {}),
        });

        if (resp.status === 503) {
          let msg = `HTTP 503`;
          try { const d = await resp.json(); msg = d.message ?? msg; } catch {}
          this._markUnhealthy(node);
          lastError = errorForStatusCode(503, msg);
          if (attempt < this._retries) {
            await _sleep(this._retryDelayMs * 2 ** attempt);
            continue;
          }
          throw lastError;
        }

        if (resp.status < 200 || resp.status >= 300) {
          let msg = `HTTP ${resp.status}`;
          try { const d = await resp.json(); msg = d.message ?? d.error ?? msg; } catch {}
          throw errorForStatusCode(resp.status, msg);
        }

        this._markHealthy(node);
        return resp;
      } catch (err) {
        if (err instanceof TorqueApiError && err.statusCode !== 503) throw err;
        this._markUnhealthy(node);

        if (err.name === 'TimeoutError' || err.name === 'AbortError') {
          lastError = new RequestTimeout(
            `Request to ${node.baseUrl}${path} timed out after ${timeoutMs}ms`,
          );
        } else if (err instanceof TorqueApiError) {
          lastError = err;
        } else {
          lastError = new TorqueError(err.message);
        }

        if (attempt < this._retries) {
          await _sleep(this._retryDelayMs * 2 ** attempt);
          continue;
        }
      }
    }

    throw lastError;
  }
}

/** @deprecated Use {@link TorqueHttpClient} instead. */
