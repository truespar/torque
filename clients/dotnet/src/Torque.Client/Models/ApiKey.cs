using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// An API key with its permissions and metadata.
/// </summary>
public sealed class ApiKey
{
    /// <summary>Unique numeric key ID.</summary>
    [JsonPropertyName("id")]
    public ulong Id { get; set; }

    /// <summary>Full key value (only present in the response to <c>POST /keys</c>).</summary>
    [JsonPropertyName("value")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Value { get; set; }

    /// <summary>First few characters of the key value for identification.</summary>
    [JsonPropertyName("value_prefix")]
    public string KeyPrefix { get; set; } = "";

    /// <summary>Human-readable description of this key's purpose.</summary>
    [JsonPropertyName("description")]
    public string Description { get; set; } = "";

    /// <summary>Permitted actions (e.g. <c>"documents:search"</c>, <c>"*"</c>).</summary>
    [JsonPropertyName("actions")]
    public List<string> Actions { get; set; } = [];

    /// <summary>Collections this key can access (e.g. <c>"*"</c> for all).</summary>
    [JsonPropertyName("collections")]
    public List<string> Collections { get; set; } = [];

    /// <summary>Unix timestamp (seconds) when this key expires, or null for no expiry.</summary>
    [JsonPropertyName("expires_at")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public ulong? ExpiresAt { get; set; }

    /// <summary>Whether to automatically delete the key after it expires.</summary>
    [JsonPropertyName("autodelete")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool Autodelete { get; set; }
}

/// <summary>
/// Request body for creating a new API key.
/// </summary>
public sealed class CreateKeyRequest
{
    /// <summary>Human-readable description of this key's purpose.</summary>
    [JsonPropertyName("description")]
    public required string Description { get; set; }

    /// <summary>Permitted actions (e.g. <c>"documents:search"</c>, <c>"*"</c>).</summary>
    [JsonPropertyName("actions")]
    public required List<string> Actions { get; set; }

    /// <summary>Collections this key can access (e.g. <c>"*"</c> for all).</summary>
    [JsonPropertyName("collections")]
    public required List<string> Collections { get; set; }

    /// <summary>Unix timestamp (seconds) when this key expires, or null for no expiry.</summary>
    [JsonPropertyName("expires_at")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public ulong? ExpiresAt { get; set; }

    /// <summary>Whether to automatically delete the key after it expires.</summary>
    [JsonPropertyName("autodelete")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingDefault)]
    public bool Autodelete { get; set; }
}
