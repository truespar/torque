using System.Text.Json;
using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// A saved search preset containing reusable search parameters.
/// </summary>
public sealed class Preset
{
    /// <summary>The preset's name.</summary>
    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    /// <summary>
    /// Preset value — either a single search parameter object or
    /// an object with a "searches" array for multi-search presets.
    /// </summary>
    [JsonPropertyName("value")]
    public JsonElement Value { get; set; }
}
