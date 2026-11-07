using System.Text.Json;
using System.Text.Json.Serialization;
using Torque.Http.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// Top-level search response containing hits, facets, and timing.
/// </summary>
public sealed class SearchResult
{
    /// <summary>Total number of documents matching the query.</summary>
    [JsonPropertyName("found")]
    public int Found { get; set; }

    /// <summary>List of matching documents for the current page.</summary>
    [JsonPropertyName("hits")]
    public List<Hit> Hits { get; set; } = [];

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
/// What a natural-language query became: the parameters the model gave and those searched.
/// </summary>
public sealed class ParsedNlQuery
{
    /// <summary>Milliseconds the model took.</summary>
    [JsonPropertyName("parse_time_ms")]
    public long ParseTimeMs { get; set; }

    /// <summary>The parameters the model gave.</summary>
    [JsonPropertyName("generated_params")]
    public Dictionary<string, JsonElement> GeneratedParams { get; set; } = new();

    /// <summary>q, filter_by and sort_by as searched.</summary>
    [JsonPropertyName("augmented_params")]
    public Dictionary<string, JsonElement> AugmentedParams { get; set; } = new();

    /// <summary>Why the query could not be translated (the search then ran as asked).</summary>
    [JsonPropertyName("error")]
    public string? Error { get; set; }

    /// <summary>The model's answer (with <c>nl_query_debug</c>).</summary>
    [JsonPropertyName("llm_response")]
    public JsonElement? LlmResponse { get; set; }
}

/// <summary>
/// A conversational search's answer.
/// </summary>
public sealed class ConversationAnswer
{
    /// <summary>The model's answer.</summary>
    [JsonPropertyName("answer")]
    public string Answer { get; set; } = "";

    /// <summary>The conversation, to continue it with <c>conversation_id</c>.</summary>
    [JsonPropertyName("conversation_id")]
    public string ConversationId { get; set; } = "";

    /// <summary>The question as searched (a follow-up as its standalone question).</summary>
    [JsonPropertyName("query")]
    public string Query { get; set; } = "";

    /// <summary>The conversation's messages so far, each <c>{"user": ...}</c> or <c>{"assistant": ...}</c> (left out with <c>exclude_fields=conversation_history</c>).</summary>
    [JsonPropertyName("conversation_history")]
    public List<Dictionary<string, string>>? ConversationHistory { get; set; }
}

/// <summary>
/// Generic search response with strongly typed hit documents.
/// </summary>
/// <typeparam name="T">The document type to deserialize each hit's document into.</typeparam>
public sealed class SearchResult<T>
{
    /// <summary>Total number of documents matching the query.</summary>
    public int Found { get; set; }

    /// <summary>List of matching documents for the current page.</summary>
    public List<Hit<T>> Hits { get; set; } = [];

    /// <summary>Facet value counts and stats, present when facet_by is specified.</summary>
    public List<FacetResult>? FacetCounts { get; set; }

    /// <summary>Server-side search time in milliseconds.</summary>
    public long SearchTimeMs { get; set; }

    /// <summary>What a natural-language query became (with <c>nl_query</c>).</summary>
    public ParsedNlQuery? ParsedNlQuery { get; set; }

    /// <summary>The answer of a conversational search (with <c>conversation</c>).</summary>
    public ConversationAnswer? Conversation { get; set; }

    internal static SearchResult<T> FromRaw(SearchResult raw, JsonSerializerOptions options)
    {
        var result = new SearchResult<T>
        {
            Found = raw.Found,
            FacetCounts = raw.FacetCounts,
            SearchTimeMs = raw.SearchTimeMs,
            ParsedNlQuery = raw.ParsedNlQuery,
            Conversation = raw.Conversation,
        };

        foreach (var hit in raw.Hits)
        {
            var json = JsonSerializer.Serialize(hit.Document);
            var doc = JsonSerializer.Deserialize<T>(json, options);
            result.Hits.Add(new Hit<T>
            {
                Document = doc!,
                TextMatch = hit.TextMatch,
                Highlights = hit.Highlights,
                GeoDistanceMeters = hit.GeoDistanceMeters,
                VectorDistance = hit.VectorDistance,
                NameScore = hit.NameScore,
            });
        }

        return result;
    }
}

/// <summary>
/// A single search hit containing the matched document and scoring metadata.
/// </summary>
public sealed class Hit
{
    /// <summary>The matched document as a field name to value dictionary.</summary>
    [JsonPropertyName("document")]
    public Dictionary<string, object?> Document { get; set; } = new();

    /// <summary>Text match score used for ranking (higher is better).</summary>
    [JsonPropertyName("text_match")]
    public ulong TextMatch { get; set; }

    /// <summary>Highlighted snippets for matched fields.</summary>
    [JsonPropertyName("highlights")]
    public List<Highlight>? Highlights { get; set; }

    /// <summary>Geo distance in meters from the query point, keyed by geopoint field name.</summary>
    [JsonPropertyName("geo_distance_meters")]
    public Dictionary<string, int>? GeoDistanceMeters { get; set; }

    /// <summary>Distance from the query vector (only present for vector search queries).</summary>
    [JsonPropertyName("vector_distance")]
    public float? VectorDistance { get; set; }

    /// <summary>In a union: the search (its place in the request) that found the hit.</summary>
    [JsonPropertyName("search_index")]
    public int? SearchIndex { get; set; }

    /// <summary>In a union: the collection of the hit.</summary>
    [JsonPropertyName("collection")]
    public string? Collection { get; set; }

    /// <summary>With <c>name_score</c>: how well the hit's names match the query's, with the evidence.</summary>
    [JsonPropertyName("name_score")]
    public NameScore? NameScore { get; set; }
}

/// <summary>
/// How well a hit's best name matches the query's name (<c>name_score</c>), between 0 and 1, with the evidence.
/// </summary>
public sealed class NameScore
{
    [JsonPropertyName("score")]
    public double Score { get; set; }

    /// <summary>The field and the name in it that scored best.</summary>
    [JsonPropertyName("field")]
    public string Field { get; set; } = "";

    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    /// <summary>The query's parts paired with the name's.</summary>
    [JsonPropertyName("pairs")]
    public List<NamePair> Pairs { get; set; } = [];

    [JsonPropertyName("unmatched_query")]
    public List<string> UnmatchedQuery { get; set; } = [];

    [JsonPropertyName("unmatched_name")]
    public List<string> UnmatchedName { get; set; } = [];
}

/// <summary>
/// A part of the query (or parts run together) paired with a part of the name.
/// </summary>
public sealed class NamePair
{
    [JsonPropertyName("query")]
    public string Query { get; set; } = "";

    [JsonPropertyName("name")]
    public string Name { get; set; } = "";

    /// <summary><c>exact</c>, <c>fold</c>, <c>typo</c>, <c>phonetic</c> or <c>initial</c>.</summary>
    [JsonPropertyName("kind")]
    public string Kind { get; set; } = "";

    /// <summary>The phonetic encoder, for kind <c>phonetic</c>.</summary>
    [JsonPropertyName("encoder")]
    public string? Encoder { get; set; }

    [JsonPropertyName("similarity")]
    public double Similarity { get; set; }
}

/// <summary>
/// A strongly typed search hit with the document deserialized to <typeparamref name="T"/>.
/// </summary>
/// <typeparam name="T">The document type.</typeparam>
public sealed class Hit<T>
{
    /// <summary>The matched document deserialized to <typeparamref name="T"/>.</summary>
    public T Document { get; set; } = default!;

    /// <summary>Text match score used for ranking (higher is better).</summary>
    public ulong TextMatch { get; set; }

    /// <summary>Highlighted snippets for matched fields.</summary>
    public List<Highlight>? Highlights { get; set; }

    /// <summary>Geo distance in meters from the query point, keyed by geopoint field name.</summary>
    public Dictionary<string, int>? GeoDistanceMeters { get; set; }

    /// <summary>Distance from the query vector (only present for vector search queries).</summary>
    public float? VectorDistance { get; set; }

    /// <summary>With <c>name_score</c>: how well the hit's names match the query's, with the evidence.</summary>
    public NameScore? NameScore { get; set; }
}

/// <summary>
/// A highlighted snippet for a matched field.
/// </summary>
public sealed class Highlight
{
    /// <summary>The field name that was highlighted.</summary>
    [JsonPropertyName("field")]
    public string Field { get; set; } = "";

    /// <summary>HTML snippet with highlighted tokens wrapped in highlight tags (a string field).</summary>
    [JsonPropertyName("snippet")]
    public string Snippet { get; set; } = "";

    /// <summary>A string[] field: the snippets of the elements that matched, and their positions in the array.</summary>
    [JsonPropertyName("snippets")]
    public List<string>? Snippets { get; set; }

    [JsonPropertyName("indices")]
    public List<int>? Indices { get; set; }

    /// <summary>The tokens that matched the query in this field (of every matching element, for a string[] field).</summary>
    [JsonPropertyName("matched_tokens")]
    [JsonConverter(typeof(MatchedTokensConverter))]
    public List<string> MatchedTokens { get; set; } = [];
}

/// <summary>
/// Facet counts and optional stats for a single faceted field.
/// </summary>
public sealed class FacetResult
{
    /// <summary>The faceted field name.</summary>
    [JsonPropertyName("field_name")]
    public string FieldName { get; set; } = "";

    /// <summary>Value counts for this faceted field.</summary>
    [JsonPropertyName("counts")]
    public List<FacetCount> Counts { get; set; } = [];

    /// <summary>The values found, and for a numeric field their min, max, sum and average.</summary>
    [JsonPropertyName("stats")]
    public FacetStats? Stats { get; set; }
}

/// <summary>
/// A single facet value and its document count.
/// </summary>
public sealed class FacetCount
{
    /// <summary>The facet value.</summary>
    [JsonPropertyName("value")]
    public string Value { get; set; } = "";

    /// <summary>Number of documents with this facet value.</summary>
    [JsonPropertyName("count")]
    public int Count { get; set; }
}

/// <summary>
/// A facet's stats: the number of values (or ranges) found, and for a numeric field the min, max,
/// sum and average of the matching documents' values.
/// </summary>
public sealed class FacetStats
{
    /// <summary>Distinct values (ranges) found.</summary>
    [JsonPropertyName("total_values")]
    public int TotalValues { get; set; }

    /// <summary>Minimum value across matching documents (numeric fields).</summary>
    [JsonPropertyName("min")]
    public double? Min { get; set; }

    /// <summary>Maximum value across matching documents (numeric fields).</summary>
    [JsonPropertyName("max")]
    public double? Max { get; set; }

    /// <summary>Sum of all values across matching documents (numeric fields).</summary>
    [JsonPropertyName("sum")]
    public double? Sum { get; set; }

    /// <summary>Average value across matching documents (numeric fields).</summary>
    [JsonPropertyName("avg")]
    public double? Avg { get; set; }
}
