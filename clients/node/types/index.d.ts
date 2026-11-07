// Type declarations for @truespar/torque-http

// ── Config types ──────────────────────────────────────────────────────────

export interface NodeConfiguration {
  host: string;
  port: number;
  protocol?: 'http' | 'https';
  path?: string;
}

export interface ClientConfiguration {
  url?: string;
  nodes?: NodeConfiguration[];
  apiKey: string;
  timeout?: number;
  transferTimeout?: number;
  retries?: number;
  retryDelayMs?: number;
  healthcheckIntervalMs?: number;
  allowUntrustedCertificate?: boolean;
  sendApiKeyAsQueryParam?: boolean;
}

// ── Schema types ──────────────────────────────────────────────────────────

export interface CollectionFieldSchema {
  name: string;
  type:
    | 'string' | 'string[]' | 'string*'
    | 'int32' | 'int32[]'
    | 'int64' | 'int64[]'
    | 'float' | 'float[]'
    | 'bool' | 'bool[]'
    | 'geopoint' | 'geopoint[]'
    | 'object' | 'object[]'
    | 'auto';
  facet?: boolean;
  optional?: boolean;
  index?: boolean;
  sort?: boolean;
  infix?: boolean;
  locale?: string;
  num_dim?: number;
  vec_dist?: 'cosine' | 'ip';
  reference?: string;
  embed?: object;
  stem?: boolean;
  drop?: boolean;
}

export interface CollectionSchema {
  name: string;
  fields: CollectionFieldSchema[];
  default_sorting_field?: string;
  enable_nested_fields?: boolean;
  token_separators?: string[];
  symbols_to_index?: string[];
  num_documents?: number;
  created_at?: number;
}

// ── Search types ──────────────────────────────────────────────────────────

export interface SearchParameters {
  q: string;
  query_by: string;
  query_by_weights?: string;
  prefix?: string;
  infix?: string;
  max_extra_prefix?: number;
  max_extra_suffix?: number;
  filter_by?: string;
  sort_by?: string;
  facet_by?: string;
  max_facet_values?: number;
  facet_query?: string;
  facet_strategy?: 'automatic' | 'exhaustive' | 'top_values';
  facet_sample_percent?: number;
  facet_sample_threshold?: number;
  facet_return_parent?: string;
  page?: number;
  per_page?: number;
  offset?: number;
  limit?: number;
  group_by?: string;
  group_limit?: number;
  group_missing_values?: boolean;
  include_fields?: string;
  exclude_fields?: string;
  highlight_fields?: string;
  highlight_full_fields?: string;
  highlight_affix_num_tokens?: number;
  highlight_start_tag?: string;
  highlight_end_tag?: string;
  snippet_threshold?: number;
  num_typos?: number;
  typo_tokens_threshold?: number;
  drop_tokens_threshold?: number;
  drop_tokens_mode?: 'right_to_left' | 'left_to_right' | `both_sides:${number}`;
  synonym_num_typos?: number;
  enable_typos_for_numerical_tokens?: boolean;
  enable_typos_for_alpha_numerical_tokens?: boolean;
  pinned_hits?: string;
  hidden_hits?: string;
  enable_overrides?: boolean;
  preset?: string;
  exhaustive_search?: boolean;
  search_cutoff_ms?: number;
  prioritize_exact_match?: boolean;
  prioritize_token_position?: boolean;
  prioritize_num_matching_fields?: boolean;
  text_match_type?: 'max_score' | 'max_weight';
  min_len_1typo?: number;
  min_len_2typo?: number;
  split_join_tokens?: 'off' | 'fallback' | 'always';
  vector_query?: string;
  synonym_prefix?: string;
  enable_synonyms?: boolean;
  synonym_sets?: string;
  stopwords?: string;
  use_cache?: boolean;
  cache_ttl?: number;
  validate_field_names?: boolean;
  max_candidates?: number;
  override_tags?: string;
  voice_query?: string;
  [key: string]: unknown;
}

export interface Highlight {
  field: string;
  snippet?: string;
  value?: string;
  matched_tokens: string[];
  indices?: number[];
}

export interface Hit {
  document: Record<string, unknown>;
  text_match: number;
  text_match_info?: object;
  highlights?: Highlight[];
  vector_distance?: number[];
  geo_distance_meters?: Record<string, number>;
}

export interface FacetCount {
  field_name: string;
  stats: {
    min?: number;
    max?: number;
    sum?: number;
    avg?: number;
    total_values: number;
  };
  counts: Array<{ value: string; count: number; highlighted: string }>;
  sampled_counts?: Array<{ value: string; count: number; highlighted: string }>;
}

export interface GroupedHit {
  group_key: unknown[];
  hits: Hit[];
  found: number;
}

export interface SearchResult {
  found: number;
  found_docs?: number;
  out_of: number;
  page: number;
  search_time_ms: number;
  search_cutoff?: boolean;
  hits: Hit[];
  facet_counts?: FacetCount[];
  grouped_hits?: GroupedHit[];
  request_params?: object;
}

// ── Document operation options ─────────────────────────────────────────────

export type DirtyValuesMode = 'coerce_or_reject' | 'coerce_or_drop' | 'drop' | 'reject';

export interface RequestOptions {
  abortSignal?: AbortSignal;
}

export interface DocumentOptions extends RequestOptions {
  dirtyValues?: DirtyValuesMode;
}

export interface ImportOptions extends DocumentOptions {
  batchSize?: number;
  returnDoc?: boolean;
  returnId?: boolean;
}

export interface ExportDocumentsParameters {
  filterBy?: string;
  includeFields?: string;
  excludeFields?: string;
}

export interface ImportDocumentsStreamOptions {
  action?: 'create' | 'upsert' | 'update' | 'emplace';
  dirtyValues?: DirtyValuesMode;
  abortSignal?: AbortSignal;
}

export interface ImportResult {
  success: boolean;
  error?: string;
  document?: Record<string, unknown>;
  id?: string;
}

// ── API key types ─────────────────────────────────────────────────────────

export interface KeySpec {
  description: string;
  actions: string[];
  collections: string[];
  value?: string;
  expires_at?: number;
}

export interface ApiKey {
  id: number;
  value: string;
  description: string;
  actions: string[];
  collections: string[];
  expires_at?: number;
}

export interface ScopedKeyParams {
  filter_by?: string;
  collection?: string;
  expires_at?: number;
  cache_ttl?: string;
  limit_multi_searches?: string;
  [key: string]: unknown;
}

// ── TorqueHttpClient ──────────────────────────────────────────────────────

export declare class TorqueHttpClient {
  constructor(config: ClientConfiguration);
  constructor(url: string, options?: Partial<ClientConfiguration>);

  // Health
  health(options?: RequestOptions): Promise<{ ok: boolean }>;
  stats(options?: RequestOptions): Promise<object>;
  metrics(options?: RequestOptions): Promise<string>;

  // Collections
  createCollection(schema: CollectionSchema, options?: RequestOptions): Promise<CollectionSchema>;
  listCollections(options?: RequestOptions): Promise<CollectionSchema[]>;
  getCollection(name: string, options?: RequestOptions): Promise<CollectionSchema>;
  updateCollection(name: string, schema: Partial<CollectionSchema>, options?: RequestOptions): Promise<CollectionSchema>;
  deleteCollection(name: string, options?: RequestOptions): Promise<void>;
  truncateCollection(name: string, options?: RequestOptions): Promise<{ num_deleted: number }>;

  // Documents — single
  createDocument(collection: string, document: object, options?: DocumentOptions): Promise<object>;
  upsertDocument(collection: string, document: object, options?: DocumentOptions): Promise<object>;
  getDocument(collection: string, documentId: string, options?: RequestOptions): Promise<object>;
  updateDocument(collection: string, documentId: string, fields: object, options?: RequestOptions): Promise<object>;
  deleteDocument(collection: string, documentId: string, options?: RequestOptions): Promise<void>;

  // Documents — bulk
  deleteByFilter(collection: string, filterBy: string, options?: RequestOptions): Promise<{ num_deleted: number }>;
  deleteDocumentsByFilter(collection: string, filterBy: string, options?: RequestOptions): Promise<{ num_deleted: number }>;
  updateDocumentsByFilter(collection: string, document: object, filterBy: string, options?: RequestOptions): Promise<{ num_updated: number }>;
  importDocuments(collection: string, documents: object[], action?: string, options?: ImportOptions): Promise<ImportResult[]>;
  importJsonl(collection: string, jsonl: string, action?: string, options?: ImportOptions): Promise<ImportResult[]>;
  exportDocuments(collection: string, params?: ExportDocumentsParameters, options?: RequestOptions): Promise<string>;
  importBinary(collection: string, filePath: string, options?: RequestOptions): Promise<object>;

  // Streaming
  exportDocumentsStream(collection: string, params?: ExportDocumentsParameters, options?: RequestOptions): AsyncGenerator<string>;
  importDocumentsStream(collection: string, readableStream: ReadableStream | AsyncIterable<unknown>, opts?: ImportDocumentsStreamOptions): Promise<ImportResult[]>;

  // Search
  search(collection: string, params?: SearchParameters, options?: RequestOptions): Promise<SearchResult>;
  multiSearch(searches: Array<{ collection: string; q: string; query_by: string; [key: string]: unknown }>, options?: RequestOptions & { commonParams?: Partial<SearchParameters> }): Promise<{ results: SearchResult[] }>;
  multiSearchUnion(searches: Array<{ collection: string; q: string; query_by: string; [key: string]: unknown }>, options?: RequestOptions & { commonParams?: Partial<SearchParameters> }): Promise<SearchResult>;

  // Aliases
  upsertAlias(name: string, collectionName: string, options?: RequestOptions): Promise<{ name: string; collection_name: string }>;
  getAlias(name: string, options?: RequestOptions): Promise<{ name: string; collection_name: string }>;
  listAliases(options?: RequestOptions): Promise<{ aliases: Array<{ name: string; collection_name: string }> }>;
  deleteAlias(name: string, options?: RequestOptions): Promise<void>;

  // API Keys
  createKey(keySpec: KeySpec, options?: RequestOptions): Promise<ApiKey>;
  listKeys(options?: RequestOptions): Promise<{ keys: ApiKey[] }>;
  getKey(id: number, options?: RequestOptions): Promise<ApiKey>;
  deleteKey(id: number, options?: RequestOptions): Promise<void>;
  generateScopedSearchKey(searchKey: string, params: ScopedKeyParams): string;

  // Synonyms
  upsertSynonymSet(name: string, body: { synonyms?: string[]; root?: string; symbols_to_index?: string[] }, options?: RequestOptions): Promise<object>;
  getSynonymSet(collection: string, name: string, options?: RequestOptions): Promise<object>;
  listSynonymSets(options?: RequestOptions): Promise<{ synonym_sets: object[] }>;
  deleteSynonymSet(name: string, options?: RequestOptions): Promise<void>;

  // Presets
  upsertPreset(id: string, body: { value: SearchParameters }, options?: RequestOptions): Promise<object>;
  getPreset(name: string, options?: RequestOptions): Promise<object>;
  listPresets(options?: RequestOptions): Promise<{ presets: object[] }>;
  deletePreset(id: string, options?: RequestOptions): Promise<void>;

  // Stopwords
  upsertStopwords(id: string, body: { stopwords: string[]; locale?: string }, options?: RequestOptions): Promise<object>;
  getStopwordsSet(name: string, options?: RequestOptions): Promise<object>;
  listStopwords(options?: RequestOptions): Promise<{ stopwords: object[] }>;
  deleteStopwords(id: string, options?: RequestOptions): Promise<void>;
}

/** @deprecated Use {@link TorqueHttpClient} instead. */

// ── TCP / binary types ────────────────────────────────────────────────────

export interface FieldDefinition {
  name: string;
  type: string;
}

export interface AckResult {
  type: 'ack';
  batchSeq: bigint;
  docsReceived: bigint;
}

export interface CommitResult {
  type: 'commit_result';
  success: boolean;
  docsIndexed: bigint;
  buildMs: bigint;
}

export declare function flattenDocument(doc: object): Record<string, unknown>;

export declare class DocumentEncoder {
  static encode(doc: object, schema: FieldDefinition[]): Buffer;
  static computeSize(doc: object, schema: FieldDefinition[]): number;
}

export declare class TorqueIngestClient {
  constructor();
  connect(host: string, port: number): Promise<void>;
  startIngest(collection: string): Promise<AckResult>;
  sendBatch(documents: object[], schema: FieldDefinition[]): Promise<AckResult>;
  commit(): Promise<CommitResult>;
  close(): void;
}

export declare class TorqueBinaryWriter {
  constructor(schema: FieldDefinition[], filePath: string);
  readonly documentCount: number;
  readonly rawSize: number;
  write(doc: object): void;
  finalize(): number;
}

// ── Error classes ─────────────────────────────────────────────────────────

export declare class TorqueError extends Error {
  constructor(message: string);
  name: 'TorqueError';
}

export declare class TorqueApiError extends TorqueError {
  constructor(message: string, statusCode: number);
  name: 'TorqueApiError';
  statusCode: number;
}

export declare class RequestMalformed extends TorqueApiError {
  constructor(message: string);
  name: 'RequestMalformed';
}

export declare class RequestUnauthorized extends TorqueApiError {
  constructor(message: string);
  name: 'RequestUnauthorized';
}

export declare class RequestForbidden extends TorqueApiError {
  constructor(message: string);
  name: 'RequestForbidden';
}

export declare class ObjectNotFound extends TorqueApiError {
  constructor(message: string);
  name: 'ObjectNotFound';
}

export declare class ObjectAlreadyExists extends TorqueApiError {
  constructor(message: string);
  name: 'ObjectAlreadyExists';
}

export declare class ServerError extends TorqueApiError {
  constructor(message: string, statusCode?: number);
  name: 'ServerError';
}

export declare class ServiceUnavailable extends TorqueApiError {
  constructor(message: string);
  name: 'ServiceUnavailable';
}

export declare class RequestTimeout extends TorqueApiError {
  constructor(message: string);
  name: 'RequestTimeout';
}

export declare function errorForStatusCode(statusCode: number, message: string): TorqueApiError;
