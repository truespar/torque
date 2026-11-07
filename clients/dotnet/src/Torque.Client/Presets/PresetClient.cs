using Torque.Http.Http;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Presets;

/// <summary>
/// Preset CRUD operations.
/// </summary>
public sealed class PresetClient
{
    private readonly HttpTransport _transport;

    internal PresetClient(HttpTransport transport) => _transport = transport;

    /// <summary>
    /// Creates or updates a search preset.
    /// </summary>
    /// <param name="id">The preset ID.</param>
    /// <param name="preset">The preset with saved search parameters.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The created or updated preset.</returns>
    public Task<Preset> UpsertAsync(string id, Preset preset, CancellationToken ct = default) =>
        _transport.PutAsync($"/presets/{Uri.EscapeDataString(id)}", preset,
            TorqueJsonContext.Default.Preset,
            TorqueJsonContext.Default.Preset, ct);

    /// <summary>
    /// Retrieves a search preset by ID.
    /// </summary>
    /// <param name="id">The preset ID.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The preset.</returns>
    public Task<Preset> GetAsync(string id, CancellationToken ct = default) =>
        _transport.GetAsync($"/presets/{Uri.EscapeDataString(id)}",
            TorqueJsonContext.Default.Preset, ct);

    /// <summary>
    /// Lists all search presets.
    /// </summary>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>A list of all presets.</returns>
    public async Task<List<Preset>> ListAsync(CancellationToken ct = default)
    {
        var wrapper = await _transport.GetAsync("/presets", TorqueJsonContext.Default.PresetsListResponse, ct);
        return wrapper.Presets;
    }

    /// <summary>
    /// Deletes a search preset by ID.
    /// </summary>
    /// <param name="id">The preset ID to delete.</param>
    /// <param name="ct">Cancellation token.</param>
    public Task DeleteAsync(string id, CancellationToken ct = default) =>
        _transport.DeleteAsync($"/presets/{Uri.EscapeDataString(id)}", ct);
}
