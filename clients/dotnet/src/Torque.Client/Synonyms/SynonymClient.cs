using Torque.Http.Http;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Synonyms;

/// <summary>
/// Synonym set and item CRUD operations.
/// Routes use /synonym_sets/{name} (not /collections/{name}/synonyms).
/// </summary>
public sealed class SynonymClient
{
    private readonly HttpTransport _transport;

    internal SynonymClient(HttpTransport transport) => _transport = transport;

    // -- Sets --

    /// <summary>
    /// Creates or updates a synonym set.
    /// </summary>
    /// <param name="name">The synonym set name.</param>
    /// <param name="set">The synonym set definition.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The created or updated synonym set.</returns>
    public Task<SynonymSet> UpsertSetAsync(string name, SynonymSet set, CancellationToken ct = default) =>
        _transport.PutAsync($"/synonym_sets/{Uri.EscapeDataString(name)}", set,
            TorqueJsonContext.Default.SynonymSet,
            TorqueJsonContext.Default.SynonymSet, ct);

    /// <summary>
    /// Retrieves a synonym set by name.
    /// </summary>
    /// <param name="name">The synonym set name.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The synonym set including all its items.</returns>
    public Task<SynonymSet> GetSetAsync(string name, CancellationToken ct = default) =>
        _transport.GetAsync($"/synonym_sets/{Uri.EscapeDataString(name)}",
            TorqueJsonContext.Default.SynonymSet, ct);

    /// <summary>
    /// Lists all synonym sets.
    /// </summary>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>A list of all synonym sets.</returns>
    public async Task<List<SynonymSet>> ListSetsAsync(CancellationToken ct = default)
    {
        var wrapper = await _transport.GetAsync("/synonym_sets", TorqueJsonContext.Default.SynonymSetsListResponse, ct);
        return wrapper.SynonymSets;
    }

    /// <summary>
    /// Deletes a synonym set and all its items.
    /// </summary>
    /// <param name="name">The synonym set name to delete.</param>
    /// <param name="ct">Cancellation token.</param>
    public Task DeleteSetAsync(string name, CancellationToken ct = default) =>
        _transport.DeleteAsync($"/synonym_sets/{Uri.EscapeDataString(name)}", ct);

    // -- Items --

    /// <summary>
    /// Creates or updates a synonym item within a set.
    /// </summary>
    /// <param name="setName">The parent synonym set name.</param>
    /// <param name="itemId">The synonym item ID.</param>
    /// <param name="item">The synonym item with synonyms list and optional root term.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The created or updated synonym item.</returns>
    public Task<SynonymItem> UpsertItemAsync(string setName, string itemId, SynonymItem item, CancellationToken ct = default) =>
        _transport.PutAsync(
            $"/synonym_sets/{Uri.EscapeDataString(setName)}/items/{Uri.EscapeDataString(itemId)}",
            item, TorqueJsonContext.Default.SynonymItem,
            TorqueJsonContext.Default.SynonymItem, ct);

    /// <summary>
    /// Retrieves a single synonym item from a set.
    /// </summary>
    /// <param name="setName">The parent synonym set name.</param>
    /// <param name="itemId">The synonym item ID.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The synonym item.</returns>
    public Task<SynonymItem> GetItemAsync(string setName, string itemId, CancellationToken ct = default) =>
        _transport.GetAsync(
            $"/synonym_sets/{Uri.EscapeDataString(setName)}/items/{Uri.EscapeDataString(itemId)}",
            TorqueJsonContext.Default.SynonymItem, ct);

    /// <summary>
    /// Lists all synonym items in a set.
    /// </summary>
    /// <param name="setName">The parent synonym set name.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>A list of all synonym items in the set.</returns>
    public Task<List<SynonymItem>> ListItemsAsync(string setName, CancellationToken ct = default) =>
        _transport.GetAsync(
            $"/synonym_sets/{Uri.EscapeDataString(setName)}/items",
            TorqueJsonContext.Default.ListSynonymItem, ct);

    /// <summary>
    /// Deletes a synonym item from a set.
    /// </summary>
    /// <param name="setName">The parent synonym set name.</param>
    /// <param name="itemId">The synonym item ID to delete.</param>
    /// <param name="ct">Cancellation token.</param>
    public Task DeleteItemAsync(string setName, string itemId, CancellationToken ct = default) =>
        _transport.DeleteAsync(
            $"/synonym_sets/{Uri.EscapeDataString(setName)}/items/{Uri.EscapeDataString(itemId)}", ct);
}
