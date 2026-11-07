using System.Runtime.CompilerServices;
using System.Text.Json;
using System.Text.Json.Serialization.Metadata;
using Torque.Http.Http;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Documents;

/// <summary>
/// Document CRUD, import/export, and bulk operations via HTTP.
/// </summary>
public sealed class DocumentClient
{
    private readonly HttpTransport _transport;

    internal DocumentClient(HttpTransport transport) => _transport = transport;

    // --- Import ---

    /// <summary>
    /// Import documents in JSONL format. Returns per-line results.
    /// </summary>
    /// <param name="collection">Target collection name.</param>
    /// <param name="documents">Documents as field dictionaries.</param>
    /// <param name="action">Import action (create, update, upsert, or emplace).</param>
    /// <param name="options">Optional import options (batch_size, return_doc, return_id, dirty_values).</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>Per-document import results indicating success or failure.</returns>
    public async Task<List<ImportResult>> ImportAsync(
        string collection,
        IEnumerable<Dictionary<string, object?>> documents,
        ImportAction action = ImportAction.Upsert,
        ImportOptions? options = null,
        CancellationToken ct = default)
    {
        var lines = string.Join("\n",
            documents.Select(d => JsonSerializer.Serialize(d)));

        var query = BuildImportQuery(action, options);
        var responseBody = await _transport.PostRawAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/import{query}",
            lines, "application/x-jsonl", ct).ConfigureAwait(false);

        return ParseImportResponse(responseBody);
    }

    /// <summary>
    /// Import documents from a streaming <see cref="IAsyncEnumerable{T}"/> source.
    /// Documents are serialized directly to the request stream — no buffering in memory.
    /// </summary>
    /// <typeparam name="T">The document type.</typeparam>
    /// <param name="collection">Target collection name.</param>
    /// <param name="documents">Async stream of documents to import.</param>
    /// <param name="typeInfo">JSON type info for serializing documents.</param>
    /// <param name="action">Import action (create, update, upsert, or emplace).</param>
    /// <param name="options">Optional import options (batch_size, return_doc, return_id, dirty_values).</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>Per-document import results indicating success or failure.</returns>
    public async Task<List<ImportResult>> ImportStreamingAsync<T>(
        string collection,
        IAsyncEnumerable<T> documents,
        JsonTypeInfo<T> typeInfo,
        ImportAction action = ImportAction.Upsert,
        ImportOptions? options = null,
        CancellationToken ct = default)
    {
        var content = new JsonLinesContent<T>(documents, typeInfo);
        content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/x-jsonl");

        var query = BuildImportQuery(action, options);
        var responseBody = await _transport.PostStreamAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/import{query}",
            content, ct).ConfigureAwait(false);

        return ParseImportResponse(responseBody);
    }

    /// <summary>
    /// Import pre-serialized JSONL lines from a streaming source.
    /// </summary>
    /// <param name="collection">Target collection name.</param>
    /// <param name="jsonLines">Async stream of pre-serialized JSON lines.</param>
    /// <param name="action">Import action (create, update, upsert, or emplace).</param>
    /// <param name="options">Optional import options (batch_size, return_doc, return_id, dirty_values).</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>Per-document import results indicating success or failure.</returns>
    public async Task<List<ImportResult>> ImportLinesAsync(
        string collection,
        IAsyncEnumerable<string> jsonLines,
        ImportAction action = ImportAction.Upsert,
        ImportOptions? options = null,
        CancellationToken ct = default)
    {
        var content = new StringLinesContent(jsonLines);
        content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/x-jsonl");

        var query = BuildImportQuery(action, options);
        var responseBody = await _transport.PostStreamAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/import{query}",
            content, ct).ConfigureAwait(false);

        return ParseImportResponse(responseBody);
    }

    // --- Export ---

    /// <summary>
    /// Export all documents from a collection as JSONL, loaded into memory.
    /// For large collections, prefer <see cref="ExportStreamingAsync"/>.
    /// </summary>
    /// <param name="collection">The collection name to export.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>All documents as JSON elements.</returns>
    public async Task<List<JsonElement>> ExportAsync(string collection, CancellationToken ct = default)
    {
        var responseBody = await _transport.GetRawAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/export", ct).ConfigureAwait(false);

        return responseBody
            .Split('\n', StringSplitOptions.RemoveEmptyEntries)
            .Select(line => JsonSerializer.Deserialize(line, TorqueJsonContext.Default.JsonElement))
            .ToList();
    }

    /// <summary>
    /// Export documents as an async stream. Each line is deserialized on demand — no full buffering.
    /// </summary>
    /// <param name="collection">The collection name to export.</param>
    /// <param name="filterBy">Optional filter expression to limit exported documents.</param>
    /// <param name="includeFields">Optional comma-separated list of fields to include.</param>
    /// <param name="excludeFields">Optional comma-separated list of fields to exclude.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>Async stream of documents as JSON elements.</returns>
    public async IAsyncEnumerable<JsonElement> ExportStreamingAsync(
        string collection,
        string? filterBy = null,
        string? includeFields = null,
        string? excludeFields = null,
        [EnumeratorCancellation] CancellationToken ct = default)
    {
        var query = BuildExportQuery(filterBy, includeFields, excludeFields);
        using var response = await _transport.GetStreamAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/export{query}", ct).ConfigureAwait(false);

        var stream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
        using var reader = new StreamReader(stream);

        while (await reader.ReadLineAsync(ct).ConfigureAwait(false) is { } line)
        {
            if (line.Length == 0) continue;
            yield return JsonSerializer.Deserialize(line, TorqueJsonContext.Default.JsonElement);
        }
    }

    // --- Single document CRUD ---

    /// <summary>
    /// Create a single document. Returns the created document.
    /// </summary>
    /// <param name="collection">Target collection name.</param>
    /// <param name="document">Document fields as a dictionary.</param>
    /// <param name="dirtyValues">How to handle type mismatches: coerce_or_reject, coerce_or_drop, drop, reject.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The created document.</returns>
    public Task<JsonElement> CreateAsync(string collection, Dictionary<string, object?> document, string? dirtyValues = null, CancellationToken ct = default)
    {
        var query = dirtyValues is not null ? $"?dirty_values={Uri.EscapeDataString(dirtyValues)}" : "";
        return _transport.PostAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents{query}",
            document, TorqueJsonContext.Default.DictionaryStringObject, TorqueJsonContext.Default.JsonElement, ct);
    }

    /// <summary>
    /// Get a document by its ID.
    /// </summary>
    /// <param name="collection">The collection name.</param>
    /// <param name="documentId">The document ID.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The document as a JSON element.</returns>
    public Task<JsonElement> GetAsync(string collection, string documentId, CancellationToken ct = default) =>
        _transport.GetAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/{Uri.EscapeDataString(documentId)}",
            TorqueJsonContext.Default.JsonElement, ct);

    /// <summary>
    /// Partially update a document by its ID. Only provided fields are changed.
    /// </summary>
    /// <param name="collection">The collection name.</param>
    /// <param name="documentId">The document ID to update.</param>
    /// <param name="fields">Fields to update as a dictionary.</param>
    /// <param name="dirtyValues">How to handle type mismatches: coerce_or_reject, coerce_or_drop, drop, reject.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The updated document.</returns>
    public Task<JsonElement> UpdateAsync(string collection, string documentId, Dictionary<string, object?> fields, string? dirtyValues = null, CancellationToken ct = default)
    {
        var query = dirtyValues is not null ? $"?dirty_values={Uri.EscapeDataString(dirtyValues)}" : "";
        return _transport.PatchAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/{Uri.EscapeDataString(documentId)}{query}",
            fields, TorqueJsonContext.Default.DictionaryStringObject, TorqueJsonContext.Default.JsonElement, ct);
    }

    /// <summary>
    /// Delete a document by its ID.
    /// </summary>
    /// <param name="collection">The collection name.</param>
    /// <param name="documentId">The document ID to delete.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The delete result.</returns>
    public Task<DeleteResult> DeleteAsync(string collection, string documentId, CancellationToken ct = default) =>
        _transport.DeleteAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/{Uri.EscapeDataString(documentId)}",
            TorqueJsonContext.Default.DeleteResult, ct);

    // --- Bulk operations ---

    /// <summary>
    /// Delete documents matching a filter. Returns the number deleted.
    /// </summary>
    /// <param name="collection">The collection name.</param>
    /// <param name="filterBy">Filter expression matching documents to delete.</param>
    /// <param name="batchSize">Optional batch size for the delete operation.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The delete result.</returns>
    public Task<DeleteResult> DeleteByFilterAsync(string collection, string filterBy, int? batchSize = null, CancellationToken ct = default)
    {
        var query = $"?filter_by={Uri.EscapeDataString(filterBy)}";
        if (batchSize.HasValue) query += $"&batch_size={batchSize.Value}";
        return _transport.DeleteAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents{query}",
            TorqueJsonContext.Default.DeleteResult, ct);
    }

    // --- Helpers ---

    private static List<ImportResult> ParseImportResponse(string responseBody) =>
        responseBody
            .Split('\n', StringSplitOptions.RemoveEmptyEntries)
            .Select(line => JsonSerializer.Deserialize(line, TorqueJsonContext.Default.ImportResult)!)
            .ToList();

    private static string ActionToString(ImportAction action) => action switch
    {
        ImportAction.Create => "create",
        ImportAction.Update => "update",
        ImportAction.Upsert => "upsert",
        ImportAction.Emplace => "emplace",
        _ => "upsert",
    };

    private static string BuildImportQuery(ImportAction action, ImportOptions? options)
    {
        // always named: the server's default (Typesense's) is create
        var parts = new List<string>(5) { $"action={ActionToString(action)}" };
        if (options is not null)
        {
            if (options.BatchSize.HasValue)
                parts.Add($"batch_size={options.BatchSize.Value}");
            if (options.ReturnDoc == true)
                parts.Add("return_doc=true");
            if (options.ReturnId == true)
                parts.Add("return_id=true");
            if (options.DirtyValues is not null)
                parts.Add($"dirty_values={Uri.EscapeDataString(options.DirtyValues)}");
        }
        return "?" + string.Join("&", parts);
    }

    private static string BuildExportQuery(string? filterBy, string? includeFields, string? excludeFields)
    {
        var parts = new List<string>(3);
        if (filterBy is not null) parts.Add($"filter_by={Uri.EscapeDataString(filterBy)}");
        if (includeFields is not null) parts.Add($"include_fields={Uri.EscapeDataString(includeFields)}");
        if (excludeFields is not null) parts.Add($"exclude_fields={Uri.EscapeDataString(excludeFields)}");
        return parts.Count == 0 ? "" : "?" + string.Join("&", parts);
    }
}

/// <summary>
/// Import action type for document import operations.
/// </summary>
public enum ImportAction
{
    /// <summary>Create new documents; fails if a document with the same ID already exists.</summary>
    Create,
    /// <summary>Update existing documents; fails if the document does not exist.</summary>
    Update,
    /// <summary>Create or update documents (default).</summary>
    Upsert,
    /// <summary>Like upsert but allows partial documents without requiring all schema fields.</summary>
    Emplace,
}
