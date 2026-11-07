using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// A named set of stopwords that can be applied to search queries to ignore common words.
/// </summary>
public sealed class StopwordSet
{
    /// <summary>Unique identifier for this stopword set.</summary>
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";

    /// <summary>Set of words to ignore during search queries.</summary>
    [JsonPropertyName("stopwords")]
    public HashSet<string> Stopwords { get; set; } = [];

    /// <summary>Language locale that restricts these stopwords to specific language queries.</summary>
    [JsonPropertyName("locale")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Locale { get; set; }
}
