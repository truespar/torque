using System.Runtime.CompilerServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Torque.Http.Aliases;
using Torque.Http.Collections;
using Torque.Http.Documents;
using Torque.Http.Http;
using Torque.Http.Keys;
using Torque.Http.Models;
using Torque.Http.Presets;
using Torque.Http.Search;
using Torque.Http.Serialization;
using Torque.Http.Stopwords;
using Torque.Http.Synonyms;

namespace Torque.Http;

/// <summary>
/// Top-level Torque HTTP API client. Entry point for all operations.
/// </summary>
public sealed class TorqueHttpClient : ITorqueHttpClient
{
    private readonly HttpClient? _ownedHttpClient;
    internal readonly HttpTransport Transport;

    private static readonly JsonSerializerOptions s_defaultJsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        PropertyNameCaseInsensitive = true,
    };

    /// <summary>Collection CRUD operations.</summary>
    public CollectionClient Collections { get; private set; } = null!;
    /// <summary>Document CRUD, import/export, and bulk operations via HTTP.</summary>
    public DocumentClient Documents { get; private set; } = null!;
    /// <summary>API key CRUD operations.</summary>
    public KeyClient Keys { get; private set; } = null!;
    /// <summary>Collection alias CRUD operations.</summary>
    public AliasClient Aliases { get; private set; } = null!;
    /// <summary>Synonym set and item CRUD operations.</summary>
    public SynonymClient Synonyms { get; private set; } = null!;
    /// <summary>Preset CRUD operations.</summary>
    public PresetClient Presets { get; private set; } = null!;
    /// <summary>Stopword set CRUD operations.</summary>
    public StopwordClient Stopwords { get; private set; } = null!;

    /// <summary>
    /// Create a standalone client with optimal connection pooling.
    /// The client owns and disposes the underlying HttpClient.
    /// </summary>
    /// <param name="options">Client configuration including API key and server nodes.</param>
    public TorqueHttpClient(TorqueHttpClientOptions options)
    {
        var handler = new SocketsHttpHandler
        {
            PooledConnectionLifetime = options.ConnectionLifetime,
            MaxConnectionsPerServer = 20,
            EnableMultipleHttp2Connections = true,
        };
        var httpClient = new HttpClient(handler) { Timeout = options.Timeout };
        _ownedHttpClient = httpClient;
        Transport = new HttpTransport(httpClient, options);
        InitSubClients();
    }

    /// <summary>
    /// Create a client with a caller-provided <see cref="HttpClient"/>.
    /// The caller owns the HttpClient lifecycle — it will not be disposed by this client.
    /// </summary>
    /// <param name="options">Client configuration including API key and server nodes.</param>
    /// <param name="httpClient">Caller-managed HttpClient instance.</param>
    public TorqueHttpClient(TorqueHttpClientOptions options, HttpClient httpClient)
    {
        _ownedHttpClient = null;
        Transport = new HttpTransport(httpClient, options);
        InitSubClients();
    }

    // Suppress definite assignment — InitSubClients sets all properties.
    private void InitSubClients()
    {
        Collections = new CollectionClient(Transport);
        Documents = new DocumentClient(Transport);
        Keys = new KeyClient(Transport);
        Aliases = new AliasClient(Transport);
        Synonyms = new SynonymClient(Transport);
        Presets = new PresetClient(Transport);
        Stopwords = new StopwordClient(Transport);
    }

    /// <summary>
    /// Create a standalone client. Same as <c>new TorqueHttpClient(options)</c>.
    /// </summary>
    /// <param name="options">Client configuration including API key and server nodes.</param>
    /// <returns>A new <see cref="TorqueHttpClient"/> instance.</returns>
    public static TorqueHttpClient Create(TorqueHttpClientOptions options) => new(options);

    /// <summary>
    /// Generate a scoped search API key from a parent search-only key and embedded parameters.
    /// The scoped key restricts searches via HMAC-SHA256 signed parameters.
    /// </summary>
    /// <param name="searchKey">The parent search-only API key value.</param>
    /// <param name="parameters">Parameters to embed (e.g. filter_by, limit_hits, expires_at).</param>
    /// <returns>A base64-encoded scoped search key.</returns>
    public static string GenerateScopedSearchKey(string searchKey, Dictionary<string, string> parameters)
    {
        var paramsJson = JsonSerializer.Serialize(parameters);
        var keyBytes = Encoding.UTF8.GetBytes(searchKey);
        var paramsBytes = Encoding.UTF8.GetBytes(paramsJson);

        Span<byte> digest = stackalloc byte[32];
        HMACSHA256.HashData(keyBytes, paramsBytes, digest);

        var digestB64 = Convert.ToBase64String(digest);
        var prefix = searchKey[..Math.Min(4, searchKey.Length)];
        var raw = $"{digestB64}{prefix}{paramsJson}";
        return Convert.ToBase64String(Encoding.UTF8.GetBytes(raw));
    }

    // --- Search ---

    /// <inheritdoc />
    public Task<SearchResult> SearchAsync(string collection, SearchParameters parameters, CancellationToken ct = default) =>
        Transport.GetAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/search{parameters.ToQueryString()}",
            TorqueJsonContext.Default.SearchResult, ct);

    /// <inheritdoc />
    public Task<SearchResult> SearchAsync(string collection, SearchBuilder builder, CancellationToken ct = default) =>
        SearchAsync(collection, builder.Build(), ct);

    /// <inheritdoc />
    public async Task<SearchResult<T>> SearchAsync<T>(string collection, SearchParameters parameters, CancellationToken ct = default)
    {
        var raw = await SearchAsync(collection, parameters, ct).ConfigureAwait(false);
        return SearchResult<T>.FromRaw(raw, s_defaultJsonOptions);
    }

    /// <inheritdoc />
    public Task<GroupedSearchResult> SearchGroupedAsync(string collection, SearchParameters parameters, CancellationToken ct = default) =>
        Transport.GetAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/search{parameters.ToQueryString()}",
            TorqueJsonContext.Default.GroupedSearchResult, ct);

    /// <inheritdoc />
    public Task<GroupedSearchResult> SearchGroupedAsync(string collection, SearchBuilder builder, CancellationToken ct = default) =>
        SearchGroupedAsync(collection, builder.Build(), ct);

    /// <inheritdoc />
    public Task<MultiSearchResponse> MultiSearchAsync(MultiSearchRequest request, Dictionary<string, string>? commonParams = null, CancellationToken ct = default)
    {
        var path = MultiSearchPath(request, commonParams);
        return Transport.PostAsync(path, request,
            TorqueJsonContext.Default.MultiSearchRequest,
            TorqueJsonContext.Default.MultiSearchResponse, ct);
    }

    /// <inheritdoc />
    public async Task<SearchResult> MultiSearchUnionAsync(MultiSearchRequest request, Dictionary<string, string>? commonParams = null, CancellationToken ct = default)
    {
        request.Union = true;
        var path = MultiSearchPath(request, commonParams);
        var body = await Transport.PostAsync(path, request,
            TorqueJsonContext.Default.MultiSearchRequest,
            TorqueJsonContext.Default.JsonElement, ct).ConfigureAwait(false);
        // a union that fails answers 200 with its code and error (as Typesense does)
        if (body.TryGetProperty("error", out var error) && body.TryGetProperty("code", out var code) && code.TryGetInt32(out var status))
            throw Exceptions.TorqueApiException.FromStatusCode((System.Net.HttpStatusCode)status, error.GetString() ?? "");
        return body.Deserialize(TorqueJsonContext.Default.SearchResult)!;
    }

    /// <summary>The multi_search path with the common parameters and the preset in the URL.</summary>
    private static string MultiSearchPath(MultiSearchRequest request, Dictionary<string, string>? commonParams)
    {
        var query = new Dictionary<string, string>(commonParams ?? []);
        if (request.Preset is { } preset)
            query["preset"] = preset;
        // a conversation's question, model and conversation are the URL's
        if (request.Q is { } q)
            query["q"] = q;
        if (request.Conversation is { } conversation)
            query["conversation"] = conversation ? "true" : "false";
        if (request.ConversationModelId is { } model)
            query["conversation_model_id"] = model;
        if (request.ConversationId is { } id)
            query["conversation_id"] = id;
        if (query.Count == 0)
            return "/multi_search";
        var qs = string.Join("&", query.Select(kv => $"{Uri.EscapeDataString(kv.Key)}={Uri.EscapeDataString(kv.Value)}"));
        return $"/multi_search?{qs}";
    }

    /// <inheritdoc />
    public Task<string> MultiSearchRawAsync(string requestJson, CancellationToken ct = default) =>
        Transport.PostRawAsync("/multi_search", requestJson, "application/json", ct);

    // --- Collection convenience ---

    /// <inheritdoc />
    public Task<CollectionResponse> UpdateCollectionAsync(string name, CollectionSchema schema, CancellationToken ct = default) =>
        Collections.UpdateAsync(name, schema, ct);

    // --- Document convenience (untyped) ---

    /// <inheritdoc />
    public Task<JsonElement> UpsertDocumentAsync(string collection, Dictionary<string, object?> document, CancellationToken ct = default) =>
        Transport.PostAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents?action=upsert",
            document, TorqueJsonContext.Default.DictionaryStringObject, TorqueJsonContext.Default.JsonElement, ct);

    /// <inheritdoc />
    public Task<DeleteResult> DeleteDocumentsByFilterAsync(string collection, string filterBy, CancellationToken ct = default) =>
        Documents.DeleteByFilterAsync(collection, filterBy, ct: ct);

    /// <inheritdoc />
    public async Task<string> UpdateDocumentsByFilterAsync(string collection, Dictionary<string, object?> document, string filterBy, CancellationToken ct = default)
    {
        var json = JsonSerializer.Serialize(document);
        return await Transport.PatchRawAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents?filter_by={Uri.EscapeDataString(filterBy)}",
            json, "application/json", ct).ConfigureAwait(false);
    }

    /// <inheritdoc />
    public Task<string> ExportDocumentsAsync(string collection, ExportOptions? options = null, CancellationToken ct = default)
    {
        var query = BuildExportQuery(options);
        return Transport.GetRawAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/export{query}", ct);
    }

    // --- Document convenience (typed generics) ---

    /// <inheritdoc />
    public async Task<T> CreateDocumentAsync<T>(string collection, T document, CancellationToken ct = default)
    {
        var json = JsonSerializer.Serialize(document, s_defaultJsonOptions);
        var responseJson = await Transport.PostRawAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents",
            json, "application/json", ct).ConfigureAwait(false);
        return JsonSerializer.Deserialize<T>(responseJson, s_defaultJsonOptions)!;
    }

    /// <inheritdoc />
    public async Task<T> UpsertDocumentAsync<T>(string collection, T document, CancellationToken ct = default)
    {
        var json = JsonSerializer.Serialize(document, s_defaultJsonOptions);
        var responseJson = await Transport.PostRawAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents?action=upsert",
            json, "application/json", ct).ConfigureAwait(false);
        return JsonSerializer.Deserialize<T>(responseJson, s_defaultJsonOptions)!;
    }

    /// <inheritdoc />
    public async Task<T> GetDocumentAsync<T>(string collection, string id, CancellationToken ct = default)
    {
        var responseJson = await Transport.GetRawAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/{Uri.EscapeDataString(id)}", ct).ConfigureAwait(false);
        return JsonSerializer.Deserialize<T>(responseJson, s_defaultJsonOptions)!;
    }

    /// <inheritdoc />
    public async Task<T> UpdateDocumentAsync<T>(string collection, string id, object partialDoc, CancellationToken ct = default)
    {
        var json = JsonSerializer.Serialize(partialDoc, s_defaultJsonOptions);
        var responseJson = await Transport.PatchRawAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/{Uri.EscapeDataString(id)}",
            json, "application/json", ct).ConfigureAwait(false);
        return JsonSerializer.Deserialize<T>(responseJson, s_defaultJsonOptions)!;
    }

    /// <inheritdoc />
    public async Task<T> DeleteDocumentAsync<T>(string collection, string id, CancellationToken ct = default)
    {
        var responseJson = await Transport.DeleteRawAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/{Uri.EscapeDataString(id)}", ct).ConfigureAwait(false);
        return JsonSerializer.Deserialize<T>(responseJson, s_defaultJsonOptions)!;
    }

    // --- Document streaming ---

    /// <inheritdoc />
    public async IAsyncEnumerable<string> ExportDocumentsStreamAsync(
        string collection, ExportOptions? options = null,
        [EnumeratorCancellation] CancellationToken ct = default)
    {
        var query = BuildExportQuery(options);
        using var response = await Transport.GetStreamAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/export{query}", ct).ConfigureAwait(false);

        var stream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
        using var reader = new StreamReader(stream);

        while (await reader.ReadLineAsync(ct).ConfigureAwait(false) is { } line)
        {
            if (line.Length == 0) continue;
            yield return line;
        }
    }

    /// <inheritdoc />
    public Task<string> ImportDocumentsStreamAsync(
        string collection, IAsyncEnumerable<string> lines, string action = "create", ImportOptions? options = null, CancellationToken ct = default)
    {
        var content = new StringLinesContent(lines);
        content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/x-jsonl");

        var parts = new List<string>(5) { $"action={Uri.EscapeDataString(action)}" };
        if (options is not null)
        {
            if (options.BatchSize.HasValue) parts.Add($"batch_size={options.BatchSize.Value}");
            if (options.ReturnDoc == true) parts.Add("return_doc=true");
            if (options.ReturnId == true) parts.Add("return_id=true");
            if (options.DirtyValues is not null) parts.Add($"dirty_values={Uri.EscapeDataString(options.DirtyValues)}");
        }
        var query = "?" + string.Join("&", parts);

        return Transport.PostStreamAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents/import{query}",
            content, ct);
    }

    // --- Individual GET helpers ---

    /// <inheritdoc />
    public Task<Preset> GetPresetAsync(string name, CancellationToken ct = default) =>
        Presets.GetAsync(name, ct);

    /// <inheritdoc />
    public Task<SynonymSet> GetSynonymSetAsync(string collection, string name, CancellationToken ct = default) =>
        Synonyms.GetSetAsync(name, ct);

    /// <inheritdoc />
    public Task<StopwordSet> GetStopwordsSetAsync(string name, CancellationToken ct = default) =>
        Stopwords.GetAsync(name, ct);

    // --- Collection operations ---

    /// <inheritdoc />
    public Task<DeleteResult> TruncateCollectionAsync(string collection, CancellationToken ct = default) =>
        Transport.DeleteAsync(
            $"/collections/{Uri.EscapeDataString(collection)}/documents",
            TorqueJsonContext.Default.DeleteResult, ct);

    // --- Admin ---

    /// <inheritdoc />
    public Task<HealthResponse> HealthAsync(CancellationToken ct = default) =>
        Transport.GetAsync("/health", TorqueJsonContext.Default.HealthResponse, ct);

    /// <inheritdoc />
    public Task<ReadyResponse> ReadyAsync(CancellationToken ct = default) =>
        Transport.GetAnyStatusAsync("/health/ready", TorqueJsonContext.Default.ReadyResponse, ct);

    /// <inheritdoc />
    public Task<AdminStats> StatsAsync(CancellationToken ct = default) =>
        Transport.GetAsync("/admin/stats", TorqueJsonContext.Default.AdminStats, ct);

    /// <inheritdoc />
    public Task<string> MetricsAsync(CancellationToken ct = default) =>
        Transport.GetRawAsync("/metrics", ct);

    /// <inheritdoc />
    public void Dispose() => _ownedHttpClient?.Dispose();

    private static string BuildExportQuery(ExportOptions? options)
    {
        if (options is null) return "";
        var parts = new List<string>(3);
        if (options.FilterBy is not null) parts.Add($"filter_by={Uri.EscapeDataString(options.FilterBy)}");
        if (options.IncludeFields is not null) parts.Add($"include_fields={Uri.EscapeDataString(options.IncludeFields)}");
        if (options.ExcludeFields is not null) parts.Add($"exclude_fields={Uri.EscapeDataString(options.ExcludeFields)}");
        return parts.Count == 0 ? "" : "?" + string.Join("&", parts);
    }
}
