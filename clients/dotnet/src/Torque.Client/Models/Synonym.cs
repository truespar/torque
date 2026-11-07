using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// A named set of synonym rules that can be applied to search queries.
/// </summary>
public sealed class SynonymSet
{
    /// <summary>Synonym set name.</summary>
    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    /// <summary>Synonym items (rules) in this set.</summary>
    [JsonPropertyName("items")]
    public List<SynonymItem> Items { get; set; } = [];
}

/// <summary>
/// A single synonym rule within a synonym set.
/// </summary>
public sealed class SynonymItem
{
    /// <summary>Unique identifier for this synonym item within the set.</summary>
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    /// <summary>List of equivalent terms (multi-way synonyms) or alternatives for the root term.</summary>
    [JsonPropertyName("synonyms")]
    public List<string> Synonyms { get; set; } = [];

    /// <summary>Root term for one-way synonyms (if set, synonyms expand searches for this root).</summary>
    [JsonPropertyName("root")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Root { get; set; }

    /// <summary>Language locale that restricts this synonym to specific language queries.</summary>
    [JsonPropertyName("locale")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Locale { get; set; }

    /// <summary>Special characters to index as part of synonym tokens.</summary>
    [JsonPropertyName("symbols_to_index")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public List<string>? SymbolsToIndex { get; set; }
}
