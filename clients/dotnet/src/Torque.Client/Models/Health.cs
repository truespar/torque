using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// Response from the <c>GET /health</c> endpoint.
/// </summary>
public sealed class HealthResponse
{
    /// <summary>Whether the server is healthy and accepting requests.</summary>
    [JsonPropertyName("ok")]
    public bool Ok { get; set; }
}

/// <summary>
/// Response from the <c>GET /ready</c> endpoint indicating index readiness.
/// </summary>
public sealed class ReadyResponse
{
    /// <summary>Whether all collections are indexed and ready to serve queries.</summary>
    [JsonPropertyName("ok")]
    public bool Ok { get; set; }

    /// <summary>Number of collections that have finished indexing.</summary>
    [JsonPropertyName("collections_ready")]
    public int CollectionsReady { get; set; }

    /// <summary>Total number of collections on the server.</summary>
    [JsonPropertyName("collections_total")]
    public int CollectionsTotal { get; set; }

    /// <summary>Human-readable status message.</summary>
    [JsonPropertyName("message")]
    public string? Message { get; set; }

    /// <summary>Convenience property equivalent to <see cref="Ok"/>.</summary>
    [JsonIgnore]
    public bool Ready => Ok;
}
