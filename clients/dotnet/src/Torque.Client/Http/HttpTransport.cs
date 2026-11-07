using System.Net;
using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization.Metadata;
using Torque.Http.Exceptions;
using Torque.Http.Serialization;

namespace Torque.Http.Http;

/// <summary>
/// HTTP transport layer with multi-node round-robin, per-request auth, retry with failover, and streaming.
/// </summary>
internal sealed class HttpTransport
{
    private readonly HttpClient _http;
    private readonly NodePool _nodePool;
    private readonly string _apiKey;
    private readonly int _maxRetries;
    private readonly int _retryBackoffMs;
    private readonly bool _sendApiKeyAsQueryParam;

    public HttpTransport(HttpClient httpClient, TorqueHttpClientOptions options)
    {
        _http = httpClient;
        _nodePool = new NodePool(options.Nodes, options.NodeCooldown);
        _apiKey = options.ApiKey;
        _maxRetries = options.MaxRetries;
        _retryBackoffMs = (int)options.RetryBackoff.TotalMilliseconds;
        _sendApiKeyAsQueryParam = options.SendApiKeyAsQueryParam;

        _http.Timeout = options.Timeout;
    }

    public async Task<T> GetAsync<T>(string path, JsonTypeInfo<T> typeInfo, CancellationToken ct = default)
    {
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Get, path, content: null, isIdempotent: true, ct).ConfigureAwait(false);
        return await DeserializeAsync(response, typeInfo, ct).ConfigureAwait(false);
    }

    public async Task<TResponse> PostAsync<TRequest, TResponse>(
        string path, TRequest body, JsonTypeInfo<TRequest> requestTypeInfo,
        JsonTypeInfo<TResponse> responseTypeInfo, CancellationToken ct = default)
    {
        var json = JsonSerializer.Serialize(body, requestTypeInfo);
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Post, path,
            () => new StringContent(json, Encoding.UTF8, "application/json"),
            isIdempotent: false, ct).ConfigureAwait(false);
        return await DeserializeAsync(response, responseTypeInfo, ct).ConfigureAwait(false);
    }

    public async Task<TResponse> PutAsync<TRequest, TResponse>(
        string path, TRequest body, JsonTypeInfo<TRequest> requestTypeInfo,
        JsonTypeInfo<TResponse> responseTypeInfo, CancellationToken ct = default)
    {
        var json = JsonSerializer.Serialize(body, requestTypeInfo);
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Put, path,
            () => new StringContent(json, Encoding.UTF8, "application/json"),
            isIdempotent: true, ct).ConfigureAwait(false);
        return await DeserializeAsync(response, responseTypeInfo, ct).ConfigureAwait(false);
    }

    public async Task DeleteAsync(string path, CancellationToken ct = default)
    {
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Delete, path, content: null, isIdempotent: true, ct).ConfigureAwait(false);
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
    }

    public async Task<T> DeleteAsync<T>(string path, JsonTypeInfo<T> typeInfo, CancellationToken ct = default)
    {
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Delete, path, content: null, isIdempotent: true, ct).ConfigureAwait(false);
        return await DeserializeAsync(response, typeInfo, ct).ConfigureAwait(false);
    }

    /// <summary>
    /// DELETE and return raw string response (for typed generic deserialization by caller).
    /// </summary>
    public async Task<string> DeleteRawAsync(string path, CancellationToken ct = default)
    {
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Delete, path, content: null, isIdempotent: true, ct).ConfigureAwait(false);
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
        return await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
    }

    public async Task<TResponse> PatchAsync<TRequest, TResponse>(
        string path, TRequest body, JsonTypeInfo<TRequest> requestTypeInfo,
        JsonTypeInfo<TResponse> responseTypeInfo, CancellationToken ct = default)
    {
        var json = JsonSerializer.Serialize(body, requestTypeInfo);
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Patch, path,
            () => new StringContent(json, Encoding.UTF8, "application/json"),
            isIdempotent: false, ct).ConfigureAwait(false);
        return await DeserializeAsync(response, responseTypeInfo, ct).ConfigureAwait(false);
    }

    /// <summary>
    /// PATCH raw string body and return raw JSON string response.
    /// </summary>
    public async Task<string> PatchRawAsync(string path, string body, string contentType, CancellationToken ct = default)
    {
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Patch, path,
            () => new StringContent(body, Encoding.UTF8, contentType),
            isIdempotent: false, ct).ConfigureAwait(false);
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
        return await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
    }

    /// <summary>
    /// POST typed request, return raw JSON string response (avoids deserialization overhead).
    /// </summary>
    public async Task<string> PostReturnRawAsync<TRequest>(
        string path, TRequest body, JsonTypeInfo<TRequest> requestTypeInfo, CancellationToken ct = default)
    {
        var json = JsonSerializer.Serialize(body, requestTypeInfo);
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Post, path,
            () => new StringContent(json, Encoding.UTF8, "application/json"),
            isIdempotent: false, ct).ConfigureAwait(false);
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
        return await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
    }

    /// <summary>
    /// POST raw string body (for JSONL import).
    /// </summary>
    public async Task<string> PostRawAsync(string path, string body, string contentType, CancellationToken ct = default)
    {
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Post, path,
            () => new StringContent(body, Encoding.UTF8, contentType),
            isIdempotent: false, ct).ConfigureAwait(false);
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
        return await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
    }

    /// <summary>
    /// POST streaming content (for IAsyncEnumerable JSONL import).
    /// </summary>
    public async Task<string> PostStreamAsync(string path, HttpContent streamContent, CancellationToken ct = default)
    {
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Post, path,
            () => streamContent,
            isIdempotent: false, ct).ConfigureAwait(false);
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
        return await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
    }

    /// <summary>
    /// PUT a file as the request body, streamed from disk (reopened when a retry resends it).
    /// </summary>
    public async Task<string> PutFileAsync(string path, string filePath, CancellationToken ct = default)
    {
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Put, path,
            () =>
            {
                var content = new StreamContent(File.OpenRead(filePath), 1 << 20);
                content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/octet-stream");
                return content;
            },
            isIdempotent: true, ct).ConfigureAwait(false);
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
        return await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
    }

    /// <summary>
    /// GET and return the response stream for line-by-line reading (streaming export).
    /// Caller must dispose the returned response.
    /// </summary>
    public async Task<HttpResponseMessage> GetStreamAsync(string path, CancellationToken ct = default)
    {
        var response = await ExecuteWithRetryAsync(
            HttpMethod.Get, path, content: null, isIdempotent: true, ct).ConfigureAwait(false);
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
        return response; // caller owns disposal
    }

    /// <summary>
    /// GET and deserialize regardless of HTTP status (for /health/ready which returns 503 with valid JSON).
    /// </summary>
    public async Task<T> GetAnyStatusAsync<T>(string path, JsonTypeInfo<T> typeInfo, CancellationToken ct = default)
    {
        var node = _nodePool.GetNext();
        var url = BuildUrl(node, path);
        if (_sendApiKeyAsQueryParam) url = AppendApiKeyParam(url);
        using var request = new HttpRequestMessage(HttpMethod.Get, url);
        if (!_sendApiKeyAsQueryParam)
            request.Headers.TryAddWithoutValidation("X-TYPESENSE-API-KEY", _apiKey);
        using var response = await _http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct).ConfigureAwait(false);
        var stream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
        return (await JsonSerializer.DeserializeAsync(stream, typeInfo, ct).ConfigureAwait(false))!;
    }

    /// <summary>
    /// GET and return raw string (for metrics endpoint).
    /// </summary>
    public async Task<string> GetRawAsync(string path, CancellationToken ct = default)
    {
        using var response = await ExecuteWithRetryAsync(
            HttpMethod.Get, path, content: null, isIdempotent: true, ct).ConfigureAwait(false);
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
        return await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
    }

    // --- Core retry + failover engine ---

    private async Task<HttpResponseMessage> ExecuteWithRetryAsync(
        HttpMethod method, string path, Func<HttpContent>? content,
        bool isIdempotent, CancellationToken ct)
    {
        int attempt = 0;
        while (true)
        {
            var node = _nodePool.GetNext();
            var url = BuildUrl(node, path);
            if (_sendApiKeyAsQueryParam) url = AppendApiKeyParam(url);

            using var request = new HttpRequestMessage(method, url);
            if (!_sendApiKeyAsQueryParam)
                request.Headers.TryAddWithoutValidation("X-TYPESENSE-API-KEY", _apiKey);
            if (content is not null)
                request.Content = content();

            HttpResponseMessage response;
            try
            {
                response = await _http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct).ConfigureAwait(false);
            }
            catch (HttpRequestException) when (attempt < _maxRetries)
            {
                _nodePool.ReportFailure(node);
                attempt++;
                await Task.Delay(ComputeBackoff(attempt), ct).ConfigureAwait(false);
                continue;
            }
            catch (TaskCanceledException) when (!ct.IsCancellationRequested && attempt < _maxRetries)
            {
                _nodePool.ReportFailure(node);
                attempt++;
                await Task.Delay(ComputeBackoff(attempt), ct).ConfigureAwait(false);
                continue;
            }

            if ((int)response.StatusCode >= 500 && attempt < _maxRetries)
            {
                _nodePool.ReportFailure(node);
                response.Dispose();
                attempt++;
                await Task.Delay(ComputeBackoff(attempt), ct).ConfigureAwait(false);
                continue;
            }

            _nodePool.ReportSuccess(node);
            return response;
        }
    }

    [MethodImpl(MethodImplOptions.AggressiveInlining)]
    private static string BuildUrl(Uri node, string path)
    {
        var baseUrl = node.ToString().TrimEnd('/');
        return $"{baseUrl}{path}";
    }

    private string AppendApiKeyParam(string url)
    {
        var separator = url.Contains('?') ? '&' : '?';
        return $"{url}{separator}x-typesense-api-key={Uri.EscapeDataString(_apiKey)}";
    }

    private int ComputeBackoff(int attempt)
    {
        int baseDelay = _retryBackoffMs * (1 << (attempt - 1));
        int jitter = Random.Shared.Next(0, baseDelay / 2 + 1);
        return Math.Min(baseDelay + jitter, 10_000);
    }

    private static async Task<T> DeserializeAsync<T>(HttpResponseMessage response, JsonTypeInfo<T> typeInfo, CancellationToken ct)
    {
        await EnsureSuccessAsync(response, ct).ConfigureAwait(false);
        var stream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
        return (await JsonSerializer.DeserializeAsync(stream, typeInfo, ct).ConfigureAwait(false))!;
    }

    private static async Task EnsureSuccessAsync(HttpResponseMessage response, CancellationToken ct)
    {
        if (response.IsSuccessStatusCode)
            return;

        var body = await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
        string message;
        try
        {
            var error = JsonSerializer.Deserialize(body, TorqueJsonContext.Default.ErrorResponse);
            message = error?.Message ?? body;
        }
        catch
        {
            message = body;
        }

        throw TorqueApiException.FromStatusCode(response.StatusCode, message);
    }
}
