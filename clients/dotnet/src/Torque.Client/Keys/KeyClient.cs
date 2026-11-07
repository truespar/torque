using Torque.Http.Http;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Keys;

/// <summary>
/// API key CRUD operations.
/// </summary>
public sealed class KeyClient
{
    private readonly HttpTransport _transport;

    internal KeyClient(HttpTransport transport) => _transport = transport;

    /// <summary>
    /// Creates a new API key with the specified actions and collection access.
    /// </summary>
    /// <param name="request">The key creation request with description, actions, and collection scope.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The created API key including the full key value (only returned on creation).</returns>
    public Task<ApiKey> CreateAsync(CreateKeyRequest request, CancellationToken ct = default) =>
        _transport.PostAsync("/keys", request,
            TorqueJsonContext.Default.CreateKeyRequest,
            TorqueJsonContext.Default.ApiKey, ct);

    /// <summary>
    /// Lists all API keys (key values are redacted to prefixes only).
    /// </summary>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>A list of all API keys.</returns>
    public async Task<List<ApiKey>> ListAsync(CancellationToken ct = default)
    {
        var wrapper = await _transport.GetAsync("/keys", TorqueJsonContext.Default.KeysListResponse, ct);
        return wrapper.Keys;
    }

    /// <summary>
    /// Retrieves a single API key by ID.
    /// </summary>
    /// <param name="id">The numeric key ID.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The API key metadata (value is redacted to prefix).</returns>
    public Task<ApiKey> GetAsync(ulong id, CancellationToken ct = default) =>
        _transport.GetAsync($"/keys/{id}", TorqueJsonContext.Default.ApiKey, ct);

    /// <summary>
    /// Deletes an API key by ID.
    /// </summary>
    /// <param name="id">The numeric key ID to delete.</param>
    /// <param name="ct">Cancellation token.</param>
    public Task DeleteAsync(ulong id, CancellationToken ct = default) =>
        _transport.DeleteAsync($"/keys/{id}", ct);
}
