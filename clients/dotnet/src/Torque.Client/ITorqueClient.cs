using System.Text.Json;
using Torque.Http.Aliases;
using Torque.Http.Collections;
using Torque.Http.Documents;
using Torque.Http.Keys;
using Torque.Http.Models;
using Torque.Http.Presets;
using Torque.Http.Search;
using Torque.Http.Stopwords;
using Torque.Http.Synonyms;

namespace Torque.Http;

/// <summary>
/// Interface for the Torque HTTP API client. Enables dependency injection and unit testing via mocking.
/// </summary>
public interface ITorqueHttpClient : IDisposable
{
    /// <summary>Collection CRUD operations.</summary>
    CollectionClient Collections { get; }

    /// <summary>Document CRUD, import/export, and bulk operations via HTTP.</summary>
    DocumentClient Documents { get; }

    /// <summary>API key CRUD operations.</summary>
    KeyClient Keys { get; }

    /// <summary>Collection alias CRUD operations.</summary>
    AliasClient Aliases { get; }

    /// <summary>Synonym set and item CRUD operations.</summary>
    SynonymClient Synonyms { get; }

    /// <summary>Preset CRUD operations.</summary>
    PresetClient Presets { get; }

    /// <summary>Stopword set CRUD operations.</summary>
    StopwordClient Stopwords { get; }

    // --- Search ---

    /// <summary>
    /// Search a collection.
    /// </summary>
    Task<SearchResult> SearchAsync(string collection, SearchParameters parameters, CancellationToken ct = default);

    /// <summary>
    /// Search a collection using a fluent builder.
    /// </summary>
    Task<SearchResult> SearchAsync(string collection, SearchBuilder builder, CancellationToken ct = default);

    /// <summary>
    /// Search a collection with strongly typed document results.
    /// </summary>
    Task<SearchResult<T>> SearchAsync<T>(string collection, SearchParameters parameters, CancellationToken ct = default);

    /// <summary>
    /// Search a collection with grouping.
    /// </summary>
    Task<GroupedSearchResult> SearchGroupedAsync(string collection, SearchParameters parameters, CancellationToken ct = default);

    /// <summary>
    /// Search a collection with grouping using a fluent builder.
    /// </summary>
    Task<GroupedSearchResult> SearchGroupedAsync(string collection, SearchBuilder builder, CancellationToken ct = default);

    /// <summary>
    /// Multi-collection search.
    /// </summary>
    Task<MultiSearchResponse> MultiSearchAsync(MultiSearchRequest request, Dictionary<string, string>? commonParams = null, CancellationToken ct = default);

    /// <summary>
    /// Multi-collection search with all hits merged into a single result set ordered by the
    /// searches' sort_by (Typesense v30.1 <c>union: true</c>), paged by the common parameters'
    /// page and per_page.
    /// </summary>
    Task<SearchResult> MultiSearchUnionAsync(MultiSearchRequest request, Dictionary<string, string>? commonParams = null, CancellationToken ct = default);

    /// <summary>
    /// Multi-collection search with zero serialization overhead.
    /// </summary>
    Task<string> MultiSearchRawAsync(string requestJson, CancellationToken ct = default);

    // --- Collection convenience ---

    /// <summary>
    /// Update a collection schema (PATCH /collections/{name}).
    /// </summary>
    Task<CollectionResponse> UpdateCollectionAsync(string name, CollectionSchema schema, CancellationToken ct = default);

    // --- Document convenience (untyped) ---

    /// <summary>
    /// Upsert a single document (create or update).
    /// </summary>
    Task<JsonElement> UpsertDocumentAsync(string collection, Dictionary<string, object?> document, CancellationToken ct = default);

    /// <summary>
    /// Delete documents matching a filter expression.
    /// </summary>
    Task<DeleteResult> DeleteDocumentsByFilterAsync(string collection, string filterBy, CancellationToken ct = default);

    /// <summary>
    /// Update documents matching a filter expression with partial field updates.
    /// </summary>
    Task<string> UpdateDocumentsByFilterAsync(string collection, Dictionary<string, object?> document, string filterBy, CancellationToken ct = default);

    /// <summary>
    /// Export all documents from a collection as JSONL.
    /// </summary>
    Task<string> ExportDocumentsAsync(string collection, ExportOptions? options = null, CancellationToken ct = default);

    // --- Document convenience (typed generics) ---

    /// <summary>
    /// Create a single document with a strongly typed model.
    /// </summary>
    Task<T> CreateDocumentAsync<T>(string collection, T document, CancellationToken ct = default);

    /// <summary>
    /// Upsert a single document with a strongly typed model.
    /// </summary>
    Task<T> UpsertDocumentAsync<T>(string collection, T document, CancellationToken ct = default);

    /// <summary>
    /// Get a document by ID with a strongly typed result.
    /// </summary>
    Task<T> GetDocumentAsync<T>(string collection, string id, CancellationToken ct = default);

    /// <summary>
    /// Partially update a document by ID and return a strongly typed result.
    /// </summary>
    Task<T> UpdateDocumentAsync<T>(string collection, string id, object partialDoc, CancellationToken ct = default);

    /// <summary>
    /// Delete a document by ID and return a strongly typed result.
    /// </summary>
    Task<T> DeleteDocumentAsync<T>(string collection, string id, CancellationToken ct = default);

    // --- Document streaming ---

    /// <summary>
    /// Export documents as an async stream of JSONL lines.
    /// </summary>
    IAsyncEnumerable<string> ExportDocumentsStreamAsync(string collection, ExportOptions? options = null, CancellationToken ct = default);

    /// <summary>
    /// Import documents from an async stream of JSONL lines.
    /// </summary>
    Task<string> ImportDocumentsStreamAsync(string collection, IAsyncEnumerable<string> lines, string action = "create", ImportOptions? options = null, CancellationToken ct = default);

    // --- Individual GET helpers ---

    /// <summary>
    /// Get a single preset by name.
    /// </summary>
    Task<Preset> GetPresetAsync(string name, CancellationToken ct = default);

    /// <summary>
    /// Get a single synonym set by name.
    /// </summary>
    Task<SynonymSet> GetSynonymSetAsync(string collection, string name, CancellationToken ct = default);

    /// <summary>
    /// Get a single stopwords set by name.
    /// </summary>
    Task<StopwordSet> GetStopwordsSetAsync(string name, CancellationToken ct = default);

    // --- Collection operations ---

    /// <summary>
    /// Truncate a collection (delete all documents).
    /// </summary>
    Task<DeleteResult> TruncateCollectionAsync(string collection, CancellationToken ct = default);

    // --- Admin ---

    /// <summary>
    /// Health check.
    /// </summary>
    Task<HealthResponse> HealthAsync(CancellationToken ct = default);

    /// <summary>
    /// Readiness check.
    /// </summary>
    Task<ReadyResponse> ReadyAsync(CancellationToken ct = default);

    /// <summary>
    /// Get admin stats (memory, collections, queries).
    /// </summary>
    Task<AdminStats> StatsAsync(CancellationToken ct = default);

    /// <summary>
    /// Get Prometheus metrics as raw text.
    /// </summary>
    Task<string> MetricsAsync(CancellationToken ct = default);
}
