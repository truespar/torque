using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// A collection alias that maps an alias name to a target collection.
/// </summary>
public sealed class CollectionAlias
{
    /// <summary>The alias name.</summary>
    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    /// <summary>The target collection name this alias points to.</summary>
    [JsonPropertyName("collection_name")]
    public string CollectionName { get; set; } = "";
}

internal sealed class UpsertAliasRequest
{
    [JsonPropertyName("collection_name")]
    public required string CollectionName { get; set; }
}
