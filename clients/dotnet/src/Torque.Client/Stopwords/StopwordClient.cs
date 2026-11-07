using Torque.Http.Http;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Stopwords;

/// <summary>
/// Stopword set CRUD operations.
/// </summary>
public sealed class StopwordClient
{
    private readonly HttpTransport _transport;

    internal StopwordClient(HttpTransport transport) => _transport = transport;

    /// <summary>
    /// Creates or updates a stopword set.
    /// </summary>
    /// <param name="id">The stopword set ID.</param>
    /// <param name="set">The stopword set with words and optional locale.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The created or updated stopword set.</returns>
    public Task<StopwordSet> UpsertAsync(string id, StopwordSet set, CancellationToken ct = default) =>
        _transport.PutAsync($"/stopwords/{Uri.EscapeDataString(id)}", set,
            TorqueJsonContext.Default.StopwordSet,
            TorqueJsonContext.Default.StopwordSet, ct);

    /// <summary>
    /// Retrieves a stopword set by ID.
    /// </summary>
    /// <param name="id">The stopword set ID.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The stopword set.</returns>
    public Task<StopwordSet> GetAsync(string id, CancellationToken ct = default) =>
        _transport.GetAsync($"/stopwords/{Uri.EscapeDataString(id)}",
            TorqueJsonContext.Default.StopwordSet, ct);

    /// <summary>
    /// Lists all stopword sets.
    /// </summary>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>A list of all stopword sets.</returns>
    public async Task<List<StopwordSet>> ListAsync(CancellationToken ct = default)
    {
        var wrapper = await _transport.GetAsync("/stopwords", TorqueJsonContext.Default.StopwordsListResponse, ct);
        return wrapper.Stopwords;
    }

    /// <summary>
    /// Deletes a stopword set by ID.
    /// </summary>
    /// <param name="id">The stopword set ID to delete.</param>
    /// <param name="ct">Cancellation token.</param>
    public Task DeleteAsync(string id, CancellationToken ct = default) =>
        _transport.DeleteAsync($"/stopwords/{Uri.EscapeDataString(id)}", ct);
}
