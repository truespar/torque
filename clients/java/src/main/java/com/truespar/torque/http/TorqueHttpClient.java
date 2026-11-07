package com.truespar.torque.http;

import tools.jackson.core.type.TypeReference;
import tools.jackson.databind.DeserializationFeature;
import tools.jackson.databind.ObjectMapper;

import javax.crypto.Mac;
import javax.crypto.spec.SecretKeySpec;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;
import java.io.IOException;
import java.io.InputStream;
import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.cert.X509Certificate;
import java.time.Duration;
import java.util.*;

/**
 * Torque search engine HTTP client. Typesense v30.1 compatible API.
 *
 * <p>Uses Java 25+ built-in {@link HttpClient} with Jackson 3 for JSON serialization.
 * Supports single-node and multi-node configurations with automatic failover,
 * retry with exponential backoff, and node health tracking.
 *
 * <h2>Single-node usage</h2>
 * <pre>{@code
 * try (var client = new TorqueHttpClient("http://localhost:8108", new TorqueHttpClientOptions("your-key"))) {
 *     var result = client.search("products", Map.of("q", "laptop", "query_by", "title"));
 * }
 * }</pre>
 *
 * <h2>Typed search with SearchParameters</h2>
 * <pre>{@code
 * try (var client = new TorqueHttpClient("http://localhost:8108", new TorqueHttpClientOptions("your-key"))) {
 *     SearchResult result = client.searchTyped("products",
 *         new SearchParameters().q("laptop").queryBy("title").filterBy("price:>100"));
 *
 *     for (Hit hit : result.hits()) {
 *         System.out.println(hit.document().get("title"));
 *     }
 * }
 * }</pre>
 *
 * <h2>Multi-node with failover</h2>
 * <pre>{@code
 * var options = new TorqueHttpClientOptions(
 *     "your-key",
 *     List.of(
 *         new Node("https", "search1.example.com", 8108),
 *         new Node("https", "search2.example.com", 8108)
 *     ),
 *     3, Duration.ofMillis(100), Duration.ofSeconds(60),
 *     Duration.ofSeconds(30), Duration.ofMinutes(5), false, false
 * );
 * try (var client = new TorqueHttpClient(options)) {
 *     var result = client.search("products", Map.of("q", "laptop", "query_by", "title"));
 * }
 * }</pre>
 *
 * @see TorqueHttpClientOptions
 * @see SearchParameters
 * @see SearchResult
 */
public class TorqueHttpClient implements AutoCloseable {
    private static final TypeReference<Map<String, Object>> MAP_TYPE = new TypeReference<>() {};
    private static final TypeReference<List<Map<String, Object>>> LIST_MAP_TYPE = new TypeReference<>() {};

    private final HttpClient http;
    private final String apiKey;
    private final Duration apiTimeout;
    private final Duration transferTimeout;
    private final ObjectMapper mapper;
    private final NodeHealthTracker healthTracker;
    private final int numRetries;
    private final Duration retryInterval;
    private final boolean sendApiKeyAsQueryParam;

    /**
     * Creates a client connected to a single Torque server.
     *
     * @param baseUrl base URL of the Torque server (e.g. {@code "http://localhost:8108"})
     * @param options client configuration including API key and timeouts
     */
    public TorqueHttpClient(String baseUrl, TorqueHttpClientOptions options) {
        this(options, List.of(URI.create(baseUrl.endsWith("/") ? baseUrl : baseUrl + "/")));
    }

    /**
     * Creates a multi-node client from options containing a list of {@link Node} configurations.
     *
     * <p>Requires at least one node in {@link TorqueHttpClientOptions#nodes()}.
     * Requests are distributed using round-robin with automatic failover to healthy nodes.
     *
     * @param options client configuration with nodes, retry settings, and API key
     * @throws IllegalArgumentException if no nodes are configured
     */
    public TorqueHttpClient(TorqueHttpClientOptions options) {
        this(options, options.nodes().stream().map(n -> URI.create(n.baseUrl())).toList());
        if (options.nodes().isEmpty()) {
            throw new IllegalArgumentException("At least one node must be configured; use TorqueHttpClient(baseUrl, options) for single-node");
        }
    }

    private TorqueHttpClient(TorqueHttpClientOptions options, List<URI> nodeUris) {
        this.apiKey = options.apiKey();
        this.apiTimeout = options.timeout();
        this.transferTimeout = options.transferTimeout();
        this.numRetries = options.numRetries();
        this.retryInterval = options.retryInterval();
        this.sendApiKeyAsQueryParam = options.sendApiKeyAsQueryParam();

        var builder = HttpClient.newBuilder();
        if (options.allowUntrustedCertificate()) {
            try {
                var ctx = SSLContext.getInstance("TLS");
                ctx.init(null, new TrustManager[]{new X509TrustManager() {
                    public X509Certificate[] getAcceptedIssuers() { return new X509Certificate[0]; }
                    public void checkClientTrusted(X509Certificate[] c, String t) {}
                    public void checkServerTrusted(X509Certificate[] c, String t) {}
                }}, null);
                builder.sslContext(ctx);
            } catch (Exception e) {
                throw new TorqueApiException("Failed to configure trust-all SSL", e);
            }
        }
        this.http = builder.build();
        this.mapper = tools.jackson.databind.json.JsonMapper.builder()
                .disable(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES)
                .build();
        this.healthTracker = new NodeHealthTracker(new ArrayList<>(nodeUris), options.healthcheckInterval());
    }

    /**
     * Closes the underlying HTTP client and releases resources.
     */
    @Override
    public void close() {
        http.close();
    }

    // ── Scoped Keys ────────────────────────────────────────────────────

    /**
     * Generates a scoped search API key from a parent search key.
     *
     * <p>Scoped keys embed search restrictions (e.g. {@code filter_by}, {@code limit_hits})
     * and are signed with HMAC-SHA256. The server validates the signature against the parent
     * key and enforces the embedded parameters on every search.
     *
     * <p>Usage:
     * <pre>{@code
     * String scopedKey = TorqueHttpClient.generateScopedSearchKey(
     *     parentSearchKeyValue,
     *     Map.of("filter_by", "tenant_id:=42", "limit_hits", "100")
     * );
     * }</pre>
     *
     * @param searchKey the parent API key value (must have {@code documents:search} action)
     * @param params    parameters to embed (e.g. {@code filter_by}, {@code exclude_fields},
     *                  {@code include_fields}, {@code limit_hits}, {@code expires_at})
     * @return base64-encoded scoped key string
     * @throws TorqueApiException if HMAC computation fails
     */
    public static String generateScopedSearchKey(String searchKey, Map<String, String> params) {
        try {
            var mapper = tools.jackson.databind.json.JsonMapper.builder().build();
            var paramsJson = mapper.writeValueAsString(params);
            var mac = Mac.getInstance("HmacSHA256");
            mac.init(new SecretKeySpec(searchKey.getBytes(StandardCharsets.UTF_8), "HmacSHA256"));
            var digest = mac.doFinal(paramsJson.getBytes(StandardCharsets.UTF_8));
            var digestB64 = Base64.getEncoder().encodeToString(digest);
            var prefix = searchKey.substring(0, Math.min(4, searchKey.length()));
            var raw = digestB64 + prefix + paramsJson;
            return Base64.getEncoder().encodeToString(raw.getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) {
            throw new TorqueApiException("Failed to generate scoped search key", e);
        }
    }

    // ── Health ──────────────────────────────────────────────────────────

    /**
     * Returns the server health status.
     *
     * @return health status map with {@code "ok"} key
     * @throws TorqueApiException if the server is unreachable or returns an error
     */
    public Map<String, Object> health() {
        return get("health");
    }

    /**
     * Returns server admin statistics including memory usage and index details.
     *
     * @return statistics map
     * @throws TorqueApiException if the request fails
     */
    public Map<String, Object> stats() {
        return get("admin/stats");
    }

    /**
     * Returns Prometheus metrics as plain text.
     *
     * @return metrics text in Prometheus exposition format
     * @throws TorqueApiException if the request fails
     */
    public String metrics() {
        return getText("metrics");
    }

    // ── Collections ────────────────────────────────────────────────────

    /**
     * Creates a new collection from a schema definition.
     *
     * @param schema collection schema as a map (must include {@code "name"} and {@code "fields"})
     * @return the created collection metadata
     * @throws ObjectAlreadyExistsException if a collection with the same name already exists
     * @throws BadRequestException          if the schema is invalid
     * @throws TorqueApiException           on other errors
     */
    public Map<String, Object> createCollection(Map<String, Object> schema) {
        return post("collections", schema);
    }

    /**
     * Lists all collections on the server.
     *
     * @return list of collection metadata maps
     * @throws TorqueApiException if the request fails
     */
    public List<Map<String, Object>> listCollections() {
        return getList("collections");
    }

    /**
     * Returns detailed metadata for a specific collection.
     *
     * @param name collection name
     * @return collection metadata including field definitions and document count
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> getCollection(String name) {
        return get("collections/" + encode(name));
    }

    /**
     * Updates a collection schema (e.g. add or drop fields).
     *
     * <p>Only the fields present in the schema map are modified; other fields remain unchanged.
     *
     * @param name   collection name
     * @param schema partial schema update (e.g. {@code Map.of("fields", List.of(...))})
     * @return the updated collection metadata
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws BadRequestException     if the schema update is invalid
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> updateCollection(String name, Map<String, Object> schema) {
        return patch("collections/" + encode(name), schema);
    }

    /**
     * Deletes a collection and all its documents.
     *
     * @param name collection name
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public void deleteCollection(String name) {
        delete("collections/" + encode(name));
    }

    /**
     * Removes all documents from a collection without dropping the schema.
     *
     * <p>Equivalent to {@code DELETE /collections/{name}/documents} with no filter.
     * The collection schema and settings are preserved; only the indexed documents
     * are removed.
     *
     * @param name collection name
     * @return response map with {@code "num_deleted"} count
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> truncateCollection(String name) {
        return deleteWithResponse("collections/" + encode(name) + "/documents");
    }

    // ── Documents ──────────────────────────────────────────────────────

    /**
     * Creates a single document in the collection.
     *
     * @param collection collection name
     * @param document   document as a key-value map (must include {@code "id"})
     * @return the created document
     * @throws ObjectAlreadyExistsException if a document with the same ID already exists
     * @throws ObjectNotFoundException      if the collection does not exist
     * @throws TorqueApiException           on other errors
     */
    public Map<String, Object> createDocument(String collection, Map<String, Object> document) {
        return post("collections/" + encode(collection) + "/documents", document);
    }

    /**
     * Creates or replaces a document (upsert). If a document with the same ID exists,
     * it is replaced entirely; otherwise a new document is created.
     *
     * @param collection collection name
     * @param document   document as a key-value map (must include {@code "id"})
     * @return the upserted document
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> upsertDocument(String collection, Map<String, Object> document) {
        return post("collections/" + encode(collection) + "/documents?action=upsert", document);
    }

    /**
     * Returns a single document by ID.
     *
     * @param collection collection name
     * @param documentId the document ID
     * @return the document as a key-value map
     * @throws ObjectNotFoundException if the document or collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> getDocument(String collection, String documentId) {
        return get("collections/" + encode(collection) + "/documents/" + encode(documentId));
    }

    /**
     * Partially updates a document by ID, merging the provided fields.
     *
     * @param collection collection name
     * @param documentId the document ID
     * @param fields     fields to update (only these fields are modified)
     * @return the updated document
     * @throws ObjectNotFoundException if the document or collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> updateDocument(String collection, String documentId, Map<String, Object> fields) {
        return patch("collections/" + encode(collection) + "/documents/" + encode(documentId), fields);
    }

    /**
     * Deletes a single document by ID.
     *
     * @param collection collection name
     * @param documentId the document ID
     * @throws ObjectNotFoundException if the document or collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public void deleteDocument(String collection, String documentId) {
        delete("collections/" + encode(collection) + "/documents/" + encode(documentId));
    }

    /**
     * Deletes all documents matching a filter expression.
     *
     * @param collection collection name
     * @param filterBy   Typesense filter expression (e.g. {@code "status:=inactive"})
     * @return response map with {@code "num_deleted"} count
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws BadRequestException     if the filter syntax is invalid
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> deleteDocumentsByFilter(String collection, String filterBy) {
        return deleteWithResponse("collections/" + encode(collection) + "/documents?filter_by=" + encode(filterBy));
    }

    /**
     * Updates all documents matching a filter expression with the provided field values.
     *
     * @param collection collection name
     * @param document   fields to update on each matching document
     * @param filterBy   Typesense filter expression (e.g. {@code "status:=draft"})
     * @return response map with {@code "num_updated"} count
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws BadRequestException     if the filter syntax is invalid
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> updateDocumentsByFilter(String collection, Map<String, Object> document, String filterBy) {
        return patch("collections/" + encode(collection) + "/documents?filter_by=" + encode(filterBy), document);
    }

    /**
     * Imports documents via JSONL (newline-delimited JSON).
     *
     * @param collection collection name
     * @param jsonl      JSONL string with one JSON document per line
     * @param action     import action: {@code "create"}, {@code "upsert"}, or {@code "update"}
     * @return per-line import results as a string
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public String importDocuments(String collection, String jsonl, String action) {
        return postRaw(
                "collections/" + encode(collection) + "/documents/import?action=" + encode(action),
                jsonl,
                "text/plain",
                transferTimeout
        );
    }

    /**
     * Imports documents via JSONL with typed parameters.
     *
     * <p>Supports all import actions including {@code "emplace"} via
     * {@link ImportDocumentsParameters#action(String)}.
     *
     * <p>Usage:
     * <pre>{@code
     * var params = new ImportDocumentsParameters()
     *     .action("emplace")
     *     .batchSize(1000)
     *     .returnId(true);
     *
     * String result = client.importDocuments("products", jsonl, params);
     * }</pre>
     *
     * @param collection collection name
     * @param jsonl      JSONL string with one JSON document per line
     * @param params     import parameters controlling action, batching, and response fields
     * @return per-line import results as a string
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public String importDocuments(String collection, String jsonl, ImportDocumentsParameters params) {
        return postRaw(
                "collections/" + encode(collection) + "/documents/import?" + params.toQueryString(),
                jsonl,
                "text/plain",
                transferTimeout
        );
    }

    /**
     * Imports documents from an {@link InputStream} by streaming the body to the server.
     *
     * <p>Suitable for large imports where the full JSONL payload should not be held in memory.
     * The stream is sent directly as the HTTP request body without buffering.
     *
     * @param collection collection name
     * @param stream     input stream of JSONL data (one JSON document per line)
     * @param params     import parameters controlling action, batching, and response fields
     * @return per-line import results as a string
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public String importDocumentsStream(String collection, InputStream stream,
                                        ImportDocumentsParameters params) {
        var path = "collections/" + encode(collection) + "/documents/import?" + params.toQueryString();
        try {
            var resp = executeWithRetry("POST", path,
                    HttpRequest.BodyPublishers.ofInputStream(() -> stream),
                    "text/plain", transferTimeout);
            return resp.body();
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("POST " + path + " failed", e); }
    }

    /**
     * Exports all documents from a collection as a JSONL string.
     *
     * @param collection collection name
     * @return JSONL string with one JSON document per line
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public String exportDocuments(String collection) {
        return getText("collections/" + encode(collection) + "/documents/export");
    }

    /**
     * Exports documents from a collection with filtering and field projection options.
     *
     * @param collection collection name
     * @param options    export options (filter, include/exclude fields)
     * @return JSONL string with one JSON document per line
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws BadRequestException     if the options are invalid
     * @throws TorqueApiException      on other errors
     */
    public String exportDocuments(String collection, ExportOptions options) {
        var qs = options.toQueryString();
        var path = "collections/" + encode(collection) + "/documents/export";
        if (!qs.isEmpty()) path += "?" + qs;
        return getText(path);
    }

    /**
     * Exports documents as a streaming {@link InputStream} for large collections.
     *
     * <p>Unlike {@link #exportDocuments(String)}, this method does not buffer the entire
     * response in memory. The caller is responsible for closing the returned stream.
     *
     * <p>Usage:
     * <pre>{@code
     * try (var stream = client.exportDocumentsStream("products", new ExportOptions())) {
     *     new BufferedReader(new InputStreamReader(stream, StandardCharsets.UTF_8))
     *         .lines()
     *         .forEach(line -> process(line));
     * }
     * }</pre>
     *
     * @param collection collection name
     * @param options    export options (filter, include/exclude fields); pass {@code null} for defaults
     * @return input stream of JSONL data (one JSON document per line)
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws BadRequestException     if the options are invalid
     * @throws TorqueApiException      on other errors
     */
    public InputStream exportDocumentsStream(String collection, ExportOptions options) {
        var path = "collections/" + encode(collection) + "/documents/export";
        if (options != null) {
            var qs = options.toQueryString();
            if (!qs.isEmpty()) path += "?" + qs;
        }
        return getStream(path);
    }

    /**
     * Uploads a TQBF binary file for high-throughput import.
     *
     * @param collection collection name
     * @param filePath   path to the {@code .tqbf} file
     * @return import result map with document count and timing
     * @throws IOException              if the file cannot be read
     * @throws ObjectNotFoundException  if the collection does not exist
     * @throws TorqueApiException       on other errors
     */
    public Map<String, Object> importBinary(String collection, Path filePath) throws IOException {
        var data = Files.readAllBytes(filePath);
        return postBytes("collections/" + encode(collection) + "/documents/import-binary", data);
    }

    // ── Search ─────────────────────────────────────────────────────────

    /**
     * Searches documents using a parameter map with Typesense wire-format keys.
     *
     * @param collection collection name
     * @param params     search parameters as key-value pairs (e.g. {@code Map.of("q", "laptop", "query_by", "title")})
     * @return raw search result map
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws BadRequestException     if the search parameters are invalid
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> search(String collection, Map<String, Object> params) {
        var sb = new StringBuilder();
        for (var entry : params.entrySet()) {
            if (entry.getValue() != null) {
                if (!sb.isEmpty()) sb.append('&');
                sb.append(encode(entry.getKey())).append('=').append(encode(String.valueOf(entry.getValue())));
            }
        }
        return get("collections/" + encode(collection) + "/documents/search?" + sb);
    }

    /**
     * Searches documents using typed {@link SearchParameters}.
     *
     * <p>This is a convenience overload that delegates to the Map-based search.
     *
     * @param collection collection name
     * @param params     typed search parameters
     * @return raw search result map
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws BadRequestException     if the search parameters are invalid
     * @throws TorqueApiException      on other errors
     * @see #searchTyped(String, SearchParameters)
     */
    public Map<String, Object> search(String collection, SearchParameters params) {
        return search(collection, params.toMap());
    }

    /**
     * Searches documents and returns a typed {@link SearchResult} with parsed hits, facets, and metadata.
     *
     * <p>Usage:
     * <pre>{@code
     * SearchResult result = client.searchTyped("products",
     *     new SearchParameters().q("laptop").queryBy("title,description").perPage(20));
     *
     * System.out.println("Found " + result.found() + " results");
     * for (Hit hit : result.hits()) {
     *     System.out.println(hit.document().get("title") + " (score: " + hit.textMatch() + ")");
     * }
     * }</pre>
     *
     * @param collection collection name
     * @param params     typed search parameters
     * @return typed search result with hits, facets, and metadata
     * @throws ObjectNotFoundException if the collection does not exist
     * @throws BadRequestException     if the search parameters are invalid
     * @throws TorqueApiException      on other errors
     * @see SearchResult
     * @see Hit
     */
    public SearchResult searchTyped(String collection, SearchParameters params) {
        var raw = search(collection, params.toMap());
        return SearchResult.fromMap(raw);
    }

    /**
     * Executes multiple searches in a single HTTP request.
     *
     * @param searches list of search parameter maps, each targeting a collection
     * @return combined results map with a {@code "results"} array
     * @throws BadRequestException if any search parameters are invalid
     * @throws TorqueApiException  on other errors
     */
    public Map<String, Object> multiSearch(List<Map<String, Object>> searches) {
        return post("multi_search", Map.of("searches", searches));
    }

    /**
     * Executes multiple searches in a single HTTP request with common parameters.
     *
     * <p>Common parameters (e.g. {@code query_by}, {@code per_page}) are applied to all
     * searches and encoded as query string parameters on the {@code /multi_search} endpoint.
     * Per-search parameters override common ones.
     *
     * @param searches     list of search parameter maps, each targeting a collection
     * @param commonParams common search parameters applied to all searches (may be {@code null})
     * @return list of result maps, one per search
     * @throws BadRequestException if any search parameters are invalid
     * @throws TorqueApiException  on other errors
     */
    @SuppressWarnings("unchecked")
    public List<Map<String, Object>> multiSearch(List<Map<String, Object>> searches,
                                                  Map<String, String> commonParams) {
        var path = "multi_search";
        if (commonParams != null && !commonParams.isEmpty()) {
            var qs = new StringBuilder();
            for (var entry : commonParams.entrySet()) {
                if (entry.getValue() != null) {
                    if (!qs.isEmpty()) qs.append('&');
                    qs.append(encode(entry.getKey())).append('=').append(encode(entry.getValue()));
                }
            }
            path += "?" + qs;
        }
        var result = post(path, Map.of("searches", searches));
        var results = result.get("results");
        if (results instanceof List<?> list) {
            return (List<Map<String, Object>>) list;
        }
        return List.of();
    }

    /**
     * Executes multiple searches merged into a single ranked result set
     * (Typesense v30.1 {@code union: true}).
     *
     * <p>Hits from all searches are merged and ranked by text match score.
     * Common parameters {@code page}/{@code per_page} paginate the merged set.
     * Not compatible with {@code group_by} searches.
     *
     * @param searches     list of search parameter maps, each targeting a collection
     * @param commonParams common search parameters applied to all searches (may be {@code null})
     * @return a single merged search result map with {@code "found"} and {@code "hits"}
     * @throws BadRequestException if any search parameters are invalid or a search uses group_by
     * @throws TorqueApiException  on other errors
     */
    public Map<String, Object> multiSearchUnion(List<Map<String, Object>> searches,
                                                Map<String, String> commonParams) {
        var path = "multi_search";
        if (commonParams != null && !commonParams.isEmpty()) {
            var qs = new StringBuilder();
            for (var entry : commonParams.entrySet()) {
                if (entry.getValue() != null) {
                    if (!qs.isEmpty()) qs.append('&');
                    qs.append(encode(entry.getKey())).append('=').append(encode(entry.getValue()));
                }
            }
            path += "?" + qs;
        }
        return post(path, Map.of("searches", searches, "union", true));
    }

    // ── Aliases ────────────────────────────────────────────────────────

    /**
     * Creates or updates a collection alias for zero-downtime schema migrations.
     *
     * @param name           alias name
     * @param collectionName target collection name
     * @return alias metadata
     * @throws ObjectNotFoundException if the target collection does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> upsertAlias(String name, String collectionName) {
        return put("aliases/" + encode(name), Map.of("collection_name", collectionName));
    }

    /**
     * Returns metadata for a specific alias.
     *
     * @param name alias name
     * @return alias metadata including target collection name
     * @throws ObjectNotFoundException if the alias does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> getAlias(String name) {
        return get("aliases/" + encode(name));
    }

    /**
     * Lists all collection aliases.
     *
     * @return map containing an {@code "aliases"} array
     * @throws TorqueApiException if the request fails
     */
    public Map<String, Object> listAliases() {
        return get("aliases");
    }

    /**
     * Deletes a collection alias.
     *
     * @param name alias name
     * @throws ObjectNotFoundException if the alias does not exist
     * @throws TorqueApiException      on other errors
     */
    public void deleteAlias(String name) {
        delete("aliases/" + encode(name));
    }

    // ── API Keys ───────────────────────────────────────────────────────

    /**
     * Creates a new API key with the specified permissions.
     *
     * @param keySpec key specification including {@code "description"}, {@code "actions"}, and {@code "collections"}
     * @return the created key (includes the key value, only shown once)
     * @throws BadRequestException if the key specification is invalid
     * @throws TorqueApiException  on other errors
     */
    public Map<String, Object> createKey(Map<String, Object> keySpec) {
        return post("keys", keySpec);
    }

    /**
     * Lists all API keys (key values are redacted).
     *
     * @return map containing a {@code "keys"} array
     * @throws TorqueApiException if the request fails
     */
    public Map<String, Object> listKeys() {
        return get("keys");
    }

    /**
     * Returns metadata for a specific API key by numeric ID.
     *
     * @param id the key ID
     * @return key metadata (value is redacted)
     * @throws ObjectNotFoundException if the key does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> getKey(long id) {
        return get("keys/" + id);
    }

    /**
     * Deletes an API key by numeric ID.
     *
     * @param id the key ID
     * @throws ObjectNotFoundException if the key does not exist
     * @throws TorqueApiException      on other errors
     */
    public void deleteKey(long id) {
        delete("keys/" + id);
    }

    // ── Synonyms ───────────────────────────────────────────────────────

    /**
     * Creates or updates a synonym set.
     *
     * @param name synonym set name
     * @param body synonym definition including {@code "synonyms"} array
     * @return the created or updated synonym set
     * @throws BadRequestException if the synonym definition is invalid
     * @throws TorqueApiException  on other errors
     */
    public Map<String, Object> upsertSynonymSet(String name, Map<String, Object> body) {
        return put("synonym_sets/" + encode(name), body);
    }

    /**
     * Returns a specific synonym set by name, optionally scoped to a collection.
     *
     * @param collection collection name (use {@code null} for global synonym sets)
     * @param name       synonym set name
     * @return synonym set metadata including the synonyms array
     * @throws ObjectNotFoundException if the synonym set does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> getSynonymSet(String collection, String name) {
        if (collection != null) {
            return get("collections/" + encode(collection) + "/synonym_sets/" + encode(name));
        }
        return get("synonym_sets/" + encode(name));
    }

    /**
     * Lists all synonym sets.
     *
     * @return map containing a {@code "synonym_sets"} array
     * @throws TorqueApiException if the request fails
     */
    public Map<String, Object> listSynonymSets() {
        return get("synonym_sets");
    }

    /**
     * Deletes a synonym set by name.
     *
     * @param name synonym set name
     * @throws ObjectNotFoundException if the synonym set does not exist
     * @throws TorqueApiException      on other errors
     */
    public void deleteSynonymSet(String name) {
        delete("synonym_sets/" + encode(name));
    }

    // ── Presets ────────────────────────────────────────────────────────

    /**
     * Creates or updates a search preset.
     *
     * @param id   preset identifier
     * @param body preset definition with search parameters
     * @return the created or updated preset
     * @throws BadRequestException if the preset definition is invalid
     * @throws TorqueApiException  on other errors
     */
    public Map<String, Object> upsertPreset(String id, Map<String, Object> body) {
        return put("presets/" + encode(id), body);
    }

    /**
     * Returns a specific search preset by name.
     *
     * @param name preset name
     * @return preset metadata including embedded search parameters
     * @throws ObjectNotFoundException if the preset does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> getPreset(String name) {
        return get("presets/" + encode(name));
    }

    /**
     * Lists all search presets.
     *
     * @return map containing a {@code "presets"} array
     * @throws TorqueApiException if the request fails
     */
    public Map<String, Object> listPresets() {
        return get("presets");
    }

    /**
     * Deletes a search preset by ID.
     *
     * @param id preset identifier
     * @throws ObjectNotFoundException if the preset does not exist
     * @throws TorqueApiException      on other errors
     */
    public void deletePreset(String id) {
        delete("presets/" + encode(id));
    }

    // ── Stopwords ──────────────────────────────────────────────────────

    /**
     * Creates or updates a stopword set.
     *
     * @param id   stopword set identifier
     * @param body stopword definition including {@code "stopwords"} array and optional {@code "locale"}
     * @return the created or updated stopword set
     * @throws BadRequestException if the stopword definition is invalid
     * @throws TorqueApiException  on other errors
     */
    public Map<String, Object> upsertStopwords(String id, Map<String, Object> body) {
        return put("stopwords/" + encode(id), body);
    }

    /**
     * Returns a specific stopwords set by name.
     *
     * @param name stopwords set name
     * @return stopwords set metadata including the stopwords array
     * @throws ObjectNotFoundException if the stopwords set does not exist
     * @throws TorqueApiException      on other errors
     */
    public Map<String, Object> getStopwordsSet(String name) {
        return get("stopwords/" + encode(name));
    }

    /**
     * Lists all stopword sets.
     *
     * @return map containing a {@code "stopwords"} array
     * @throws TorqueApiException if the request fails
     */
    public Map<String, Object> listStopwords() {
        return get("stopwords");
    }

    /**
     * Deletes a stopword set by ID.
     *
     * @param id stopword set identifier
     * @throws ObjectNotFoundException if the stopword set does not exist
     * @throws TorqueApiException      on other errors
     */
    public void deleteStopwords(String id) {
        delete("stopwords/" + encode(id));
    }

    // ── Internal HTTP helpers with retry + failover ───────────────────

    private HttpRequest.Builder request(URI base, String path) {
        URI uri;
        if (sendApiKeyAsQueryParam) {
            var sep = path.contains("?") ? "&" : "?";
            uri = base.resolve(path + sep + "x-typesense-api-key=" + encode(apiKey));
        } else {
            uri = base.resolve(path);
        }
        var builder = HttpRequest.newBuilder().uri(uri);
        if (!sendApiKeyAsQueryParam) {
            builder.header("X-TYPESENSE-API-KEY", apiKey);
        }
        return builder;
    }

    /**
     * Executes an HTTP request with retry and multi-node failover.
     *
     * <p>On failure (network error or 5xx), the node is marked unhealthy and the
     * request is retried on the next healthy node. Retries use exponential backoff
     * starting from {@link TorqueHttpClientOptions#retryInterval()}.
     *
     * <p>Client errors (4xx) are never retried — they indicate a problem with the
     * request itself, not the server.
     */
    private HttpResponse<String> executeWithRetry(String method, String path,
                                                   HttpRequest.BodyPublisher body,
                                                   String contentType,
                                                   Duration timeout) {
        TorqueApiException lastException = null;
        int maxAttempts = numRetries + 1;

        for (int attempt = 0; attempt < maxAttempts; attempt++) {
            URI nodeUri = healthTracker.nextNode();
            try {
                var reqBuilder = request(nodeUri, path).timeout(timeout);
                if (contentType != null) {
                    reqBuilder.header("Content-Type", contentType);
                }
                var req = switch (method) {
                    case "GET" -> reqBuilder.GET().build();
                    case "POST" -> reqBuilder.POST(body).build();
                    case "PUT" -> reqBuilder.PUT(body).build();
                    case "DELETE" -> reqBuilder.DELETE().build();
                    case "PATCH" -> reqBuilder.method("PATCH", body).build();
                    default -> throw new IllegalArgumentException("Unknown method: " + method);
                };

                var resp = http.send(req, HttpResponse.BodyHandlers.ofString());

                // 4xx errors are client errors — do not retry
                if (resp.statusCode() >= 400 && resp.statusCode() < 500) {
                    healthTracker.markHealthy(nodeUri);
                    throwMappedException(resp);
                }

                // 5xx errors — mark node unhealthy and retry
                if (resp.statusCode() >= 500) {
                    healthTracker.markUnhealthy(nodeUri);
                    lastException = buildException(resp);
                    backoff(attempt);
                    continue;
                }

                // Success
                healthTracker.markHealthy(nodeUri);
                return resp;

            } catch (TorqueApiException e) {
                // Re-throw client errors (4xx) immediately
                if (e.statusCode() != null && e.statusCode() < 500) throw e;
                healthTracker.markUnhealthy(nodeUri);
                lastException = e;
                backoff(attempt);
            } catch (IOException | InterruptedException e) {
                healthTracker.markUnhealthy(nodeUri);
                lastException = new TorqueApiException(method + " " + path + " failed: " + e.getMessage(), e);
                backoff(attempt);
                if (e instanceof InterruptedException) Thread.currentThread().interrupt();
            }
        }

        throw lastException != null ? lastException
                : new TorqueApiException(method + " " + path + " failed after " + maxAttempts + " attempts");
    }

    /**
     * Executes a GET request that returns an InputStream, with retry and multi-node failover.
     * Follows the same retry semantics as {@link #executeWithRetry}.
     */
    private HttpResponse<InputStream> executeStreamWithRetry(String path, Duration timeout) {
        TorqueApiException lastException = null;
        int maxAttempts = numRetries + 1;

        for (int attempt = 0; attempt < maxAttempts; attempt++) {
            URI nodeUri = healthTracker.nextNode();
            try {
                var req = request(nodeUri, path).timeout(timeout).GET().build();
                var resp = http.send(req, HttpResponse.BodyHandlers.ofInputStream());

                if (resp.statusCode() >= 400 && resp.statusCode() < 500) {
                    healthTracker.markHealthy(nodeUri);
                    // Read the error body to produce a good exception message
                    String errorBody;
                    try (var is = resp.body()) {
                        errorBody = new String(is.readAllBytes(), StandardCharsets.UTF_8);
                    }
                    throw buildExceptionFromRaw(resp.statusCode(), errorBody);
                }

                if (resp.statusCode() >= 500) {
                    healthTracker.markUnhealthy(nodeUri);
                    String errorBody;
                    try (var is = resp.body()) {
                        errorBody = new String(is.readAllBytes(), StandardCharsets.UTF_8);
                    }
                    lastException = buildExceptionFromRaw(resp.statusCode(), errorBody);
                    backoff(attempt);
                    continue;
                }

                healthTracker.markHealthy(nodeUri);
                return resp;

            } catch (TorqueApiException e) {
                if (e.statusCode() != null && e.statusCode() < 500) throw e;
                healthTracker.markUnhealthy(nodeUri);
                lastException = e;
                backoff(attempt);
            } catch (IOException | InterruptedException e) {
                healthTracker.markUnhealthy(nodeUri);
                lastException = new TorqueApiException("GET " + path + " stream failed: " + e.getMessage(), e);
                backoff(attempt);
                if (e instanceof InterruptedException) Thread.currentThread().interrupt();
            }
        }

        throw lastException != null ? lastException
                : new TorqueApiException("GET " + path + " stream failed after " + maxAttempts + " attempts");
    }

    private void backoff(int attempt) {
        if (attempt >= numRetries) return;
        try {
            long millis = retryInterval.toMillis() * (1L << attempt);
            Thread.sleep(millis);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    private Map<String, Object> get(String path) {
        try {
            var resp = executeWithRetry("GET", path, null, null, apiTimeout);
            return mapper.readValue(resp.body(), MAP_TYPE);
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("GET " + path + " failed", e); }
    }

    private List<Map<String, Object>> getList(String path) {
        try {
            var resp = executeWithRetry("GET", path, null, null, apiTimeout);
            return mapper.readValue(resp.body(), LIST_MAP_TYPE);
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("GET " + path + " failed", e); }
    }

    private String getText(String path) {
        try {
            var resp = executeWithRetry("GET", path, null, null, apiTimeout);
            return resp.body();
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("GET " + path + " failed", e); }
    }

    private InputStream getStream(String path) {
        try {
            var resp = executeStreamWithRetry(path, transferTimeout);
            return resp.body();
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("GET " + path + " stream failed", e); }
    }

    private Map<String, Object> post(String path, Object body) {
        try {
            var json = mapper.writeValueAsString(body);
            var resp = executeWithRetry("POST", path,
                    HttpRequest.BodyPublishers.ofString(json), "application/json", apiTimeout);
            var text = resp.body();
            return text.isBlank() ? Map.of() : mapper.readValue(text, MAP_TYPE);
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("POST " + path + " failed", e); }
    }

    private String postRaw(String path, String body, String contentType, Duration timeout) {
        try {
            var resp = executeWithRetry("POST", path,
                    HttpRequest.BodyPublishers.ofString(body), contentType, timeout);
            return resp.body();
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("POST " + path + " failed", e); }
    }

    private Map<String, Object> postBytes(String path, byte[] body) {
        try {
            var resp = executeWithRetry("POST", path,
                    HttpRequest.BodyPublishers.ofByteArray(body), "application/octet-stream", transferTimeout);
            return mapper.readValue(resp.body(), MAP_TYPE);
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("POST " + path + " failed", e); }
    }

    private Map<String, Object> put(String path, Object body) {
        try {
            var json = mapper.writeValueAsString(body);
            var resp = executeWithRetry("PUT", path,
                    HttpRequest.BodyPublishers.ofString(json), "application/json", apiTimeout);
            var text = resp.body();
            return text.isBlank() ? Map.of() : mapper.readValue(text, MAP_TYPE);
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("PUT " + path + " failed", e); }
    }

    private Map<String, Object> patch(String path, Object body) {
        try {
            var json = mapper.writeValueAsString(body);
            var resp = executeWithRetry("PATCH", path,
                    HttpRequest.BodyPublishers.ofString(json), "application/json", apiTimeout);
            var text = resp.body();
            return text.isBlank() ? Map.of() : mapper.readValue(text, MAP_TYPE);
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("PATCH " + path + " failed", e); }
    }

    private void delete(String path) {
        try {
            executeWithRetry("DELETE", path, null, null, apiTimeout);
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("DELETE " + path + " failed", e); }
    }

    private Map<String, Object> deleteWithResponse(String path) {
        try {
            var resp = executeWithRetry("DELETE", path, null, null, apiTimeout);
            var text = resp.body();
            return text.isBlank() ? Map.of() : mapper.readValue(text, MAP_TYPE);
        } catch (TorqueApiException e) { throw e; }
        catch (Exception e) { throw new TorqueApiException("DELETE " + path + " failed", e); }
    }

    // ── Error mapping ─────────────────────────────────────────────────

    /**
     * Maps an HTTP error response to the appropriate typed exception and throws it.
     */
    private void throwMappedException(HttpResponse<String> resp) {
        throw buildException(resp);
    }

    /**
     * Builds a typed exception from an HTTP error response, mapping status codes
     * to specific exception classes.
     */
    private TorqueApiException buildException(HttpResponse<String> resp) {
        String msg = extractErrorMessage(resp);
        return switch (resp.statusCode()) {
            case 400 -> new BadRequestException(msg);
            case 401 -> new UnauthorizedException(msg);
            case 403 -> new ForbiddenException(msg);
            case 404 -> new ObjectNotFoundException(msg);
            case 409 -> new ObjectAlreadyExistsException(msg);
            case 503 -> new ServiceUnavailableException(msg);
            default -> {
                if (resp.statusCode() >= 500) yield new ServerErrorException(msg, resp.statusCode());
                yield new TorqueApiException(msg, resp.statusCode());
            }
        };
    }

    /**
     * Builds a typed exception from a raw status code and body string.
     * Used by the streaming retry path where we don't have an HttpResponse&lt;String&gt;.
     */
    private TorqueApiException buildExceptionFromRaw(int statusCode, String body) {
        String msg;
        try {
            var data = mapper.readValue(body, MAP_TYPE);
            var message = data.get("message");
            if (message == null) message = data.get("error");
            msg = message != null ? message.toString()
                    : "HTTP " + statusCode + ": " + body.substring(0, Math.min(200, body.length()));
        } catch (Exception ignored) {
            msg = "HTTP " + statusCode + ": " + body.substring(0, Math.min(200, body.length()));
        }
        return switch (statusCode) {
            case 400 -> new BadRequestException(msg);
            case 401 -> new UnauthorizedException(msg);
            case 403 -> new ForbiddenException(msg);
            case 404 -> new ObjectNotFoundException(msg);
            case 409 -> new ObjectAlreadyExistsException(msg);
            case 503 -> new ServiceUnavailableException(msg);
            default -> {
                if (statusCode >= 500) yield new ServerErrorException(msg, statusCode);
                yield new TorqueApiException(msg, statusCode);
            }
        };
    }

    /**
     * Extracts a human-readable error message from the server JSON response,
     * falling back to the raw HTTP status and body.
     */
    private String extractErrorMessage(HttpResponse<String> resp) {
        try {
            var data = mapper.readValue(resp.body(), MAP_TYPE);
            var message = data.get("message");
            if (message == null) message = data.get("error");
            if (message != null) return message.toString();
        } catch (Exception ignored) {}
        return "HTTP " + resp.statusCode() + ": "
                + resp.body().substring(0, Math.min(200, resp.body().length()));
    }

    private static String encode(String s) {
        return URLEncoder.encode(s, StandardCharsets.UTF_8).replace("+", "%20");
    }
}
