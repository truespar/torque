using System.Text.Json;
using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// Search response with results grouped by a field value.
/// </summary>
public sealed class GroupedSearchResult
{
    /// <summary>Total number of groups matching the query.</summary>
    [JsonPropertyName("found")]
    public int Found { get; set; }

    /// <summary>Total number of documents matching the query across all groups.</summary>
    [JsonPropertyName("found_docs")]
    public int FoundDocs { get; set; }

    /// <summary>List of hit groups, each containing documents sharing the same group key.</summary>
    [JsonPropertyName("grouped_hits")]
    public List<GroupHit> GroupedHits { get; set; } = [];

    /// <summary>Facet value counts and stats, present when facet_by is specified.</summary>
    [JsonPropertyName("facet_counts")]
    public List<FacetResult>? FacetCounts { get; set; }

    /// <summary>Server-side search time in milliseconds.</summary>
    [JsonPropertyName("search_time_ms")]
    public long SearchTimeMs { get; set; }

    /// <summary>What a natural-language query became (with <c>nl_query</c>).</summary>
    [JsonPropertyName("parsed_nl_query")]
    public ParsedNlQuery? ParsedNlQuery { get; set; }

    /// <summary>The answer of a conversational search (with <c>conversation</c>).</summary>
    [JsonPropertyName("conversation")]
    public ConversationAnswer? Conversation { get; set; }
}

/// <summary>
/// A single group of search hits sharing the same group key value.
/// </summary>
public sealed class GroupHit
{
    /// <summary>The group key values for this group.</summary>
    [JsonPropertyName("group_key")]
    public List<JsonElement> GroupKey { get; set; } = [];

    /// <summary>Documents in this group.</summary>
    [JsonPropertyName("hits")]
    public List<Hit> Hits { get; set; } = [];

    /// <summary>Total number of documents in this group.</summary>
    [JsonPropertyName("found")]
    public int Found { get; set; }
}
