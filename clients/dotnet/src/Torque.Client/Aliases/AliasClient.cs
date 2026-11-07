using Torque.Http.Http;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Aliases;

/// <summary>
/// Collection alias CRUD operations.
/// </summary>
public sealed class AliasClient
{
    private readonly HttpTransport _transport;

    internal AliasClient(HttpTransport transport) => _transport = transport;

    /// <summary>
    /// Creates or updates a collection alias pointing to the specified collection.
    /// </summary>
    /// <param name="name">The alias name.</param>
    /// <param name="collectionName">The target collection name.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The created or updated alias.</returns>
    public Task<CollectionAlias> UpsertAsync(string name, string collectionName, CancellationToken ct = default) =>
        _transport.PutAsync($"/aliases/{Uri.EscapeDataString(name)}",
            new UpsertAliasRequest { CollectionName = collectionName },
            TorqueJsonContext.Default.UpsertAliasRequest,
            TorqueJsonContext.Default.CollectionAlias, ct);

    /// <summary>
    /// Retrieves a single alias by name.
    /// </summary>
    /// <param name="name">The alias name.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The alias with its target collection name.</returns>
    public Task<CollectionAlias> GetAsync(string name, CancellationToken ct = default) =>
        _transport.GetAsync($"/aliases/{Uri.EscapeDataString(name)}",
            TorqueJsonContext.Default.CollectionAlias, ct);

    /// <summary>
    /// Lists all collection aliases.
    /// </summary>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>A list of all aliases.</returns>
    public async Task<List<CollectionAlias>> ListAsync(CancellationToken ct = default)
    {
        var wrapper = await _transport.GetAsync("/aliases", TorqueJsonContext.Default.AliasesListResponse, ct);
        return wrapper.Aliases;
    }

    /// <summary>
    /// Deletes a collection alias.
    /// </summary>
    /// <param name="name">The alias name to delete.</param>
    /// <param name="ct">Cancellation token.</param>
    public Task DeleteAsync(string name, CancellationToken ct = default) =>
        _transport.DeleteAsync($"/aliases/{Uri.EscapeDataString(name)}", ct);
}
