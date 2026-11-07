namespace Torque.Http;

/// <summary>
/// Configuration options for <see cref="TorqueHttpClient"/>.
/// </summary>
public sealed class TorqueHttpClientOptions
{
    /// <summary>
    /// API key for authentication (X-TYPESENSE-API-KEY header).
    /// </summary>
    public required string ApiKey { get; set; }

    /// <summary>
    /// Server nodes to connect to. Requests are distributed via round-robin with health tracking.
    /// At least one node is required. Example: <c>new Uri("http://localhost:8108")</c>.
    /// </summary>
    public List<Uri> Nodes { get; set; } = [new("http://localhost:8108")];

    /// <summary>
    /// Convenience property: set a single base URL (e.g. "http://localhost:8108").
    /// Replaces the <see cref="Nodes"/> list with a single node.
    /// </summary>
    public string BaseUrl
    {
        get => Nodes.Count > 0 ? Nodes[0].ToString().TrimEnd('/') : "http://localhost:8108";
        set => Nodes = [new Uri(value.TrimEnd('/'))];
    }

    /// <summary>
    /// HTTP request timeout (default: 30 seconds).
    /// </summary>
    public TimeSpan Timeout { get; set; } = TimeSpan.FromSeconds(30);

    /// <summary>
    /// Maximum number of retries for transient HTTP failures (default: 2).
    /// On multi-node setups, retries cycle through different nodes.
    /// </summary>
    public int MaxRetries { get; set; } = 2;

    /// <summary>
    /// Initial backoff delay for retries (default: 200ms). Doubles with each retry, capped at 10s.
    /// </summary>
    public TimeSpan RetryBackoff { get; set; } = TimeSpan.FromMilliseconds(200);

    /// <summary>
    /// Duration before an unhealthy node is retried (default: 60 seconds).
    /// </summary>
    public TimeSpan NodeCooldown { get; set; } = TimeSpan.FromSeconds(60);

    /// <summary>
    /// Connection pool lifetime for DNS refresh (default: 2 minutes).
    /// Only used when creating an HttpClient via <see cref="TorqueHttpClient.Create"/>.
    /// </summary>
    public TimeSpan ConnectionLifetime { get; set; } = TimeSpan.FromMinutes(2);

    /// <summary>
    /// When true, the API key is sent as the <c>x-typesense-api-key</c> query parameter
    /// instead of the <c>X-TYPESENSE-API-KEY</c> request header.
    /// Use when a proxy or environment strips custom headers (default: false).
    /// </summary>
    public bool SendApiKeyAsQueryParam { get; set; } = false;
}
