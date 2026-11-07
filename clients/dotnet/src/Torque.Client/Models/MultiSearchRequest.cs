using System.Text.Json;
using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// Request body for multi-collection search containing multiple search queries.
/// </summary>
public sealed class MultiSearchRequest
{
    /// <summary>List of search queries to execute across collections.</summary>
    [JsonPropertyName("searches")]
    public required List<MultiSearchQuery> Searches { get; set; }

    /// <summary>
    /// A preset named in the URL: a multi-search preset's searches are the request (the body's
    /// are then ignored); a single preset gives every search the parameters it lacks.
    /// </summary>
    [JsonIgnore]
    public string? Preset { get; set; }

    /// <summary>
    /// When true, all search results are merged into a single result set ordered by the
    /// searches' sort_by (which must agree in fields, types and orders) instead of one result
    /// per search; paged by the URL's page and per_page. Use <c>MultiSearchUnionAsync</c>.
    /// Every search grouped or none.
    /// </summary>
    [JsonPropertyName("union")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? Union { get; set; }

    /// <summary>
    /// A union: whether a document found by several searches of its collection appears (and
    /// counts) once (default true).
    /// </summary>
    [JsonPropertyName("remove_duplicates")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? RemoveDuplicates { get; set; }

    /// <summary>
    /// The question of a conversation over every search's results (sent in the URL as
    /// <c>q</c>, with <see cref="Conversation"/>).
    /// </summary>
    [JsonIgnore]
    public string? Q { get; set; }

    /// <summary>Answer <see cref="Q"/> from every search's hits; the answer is in <c>conversation</c>.</summary>
    [JsonIgnore]
    public bool? Conversation { get; set; }

    /// <summary>The conversation model answering (required with <see cref="Conversation"/>).</summary>
    [JsonIgnore]
    public string? ConversationModelId { get; set; }

    /// <summary>A conversation to continue.</summary>
    [JsonIgnore]
    public string? ConversationId { get; set; }
}

/// <summary>
/// A single search query within a multi-search request.
/// </summary>
public sealed class MultiSearchQuery
{
    /// <summary>Target collection name.</summary>
    [JsonPropertyName("collection")]
    public required string Collection { get; set; }

    /// <summary>Search query text.</summary>
    [JsonPropertyName("q")]
    public string? Q { get; set; }

    /// <summary>Comma-separated list of fields to search.</summary>
    [JsonPropertyName("query_by")]
    public string? QueryBy { get; set; }

    /// <summary>Filter expression to narrow results.</summary>
    [JsonPropertyName("filter_by")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? FilterBy { get; set; }

    /// <summary>Comma-separated sort expression.</summary>
    [JsonPropertyName("sort_by")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? SortBy { get; set; }

    /// <summary>Comma-separated list of fields to facet on.</summary>
    [JsonPropertyName("facet_by")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? FacetBy { get; set; }

    /// <summary>Number of results per page.</summary>
    [JsonPropertyName("per_page")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? PerPage { get; set; }

    /// <summary>Page number for paginated results (1-based).</summary>
    [JsonPropertyName("page")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? Page { get; set; }

    /// <summary>Field name to group results by.</summary>
    [JsonPropertyName("group_by")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? GroupBy { get; set; }

    /// <summary>Maximum number of hits returned per group.</summary>
    [JsonPropertyName("group_limit")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? GroupLimit { get; set; }

    /// <summary>Preset ID whose saved parameters are merged with this query.</summary>
    [JsonPropertyName("preset")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? Preset { get; set; }

    /// <summary>Maximum number of facet values to return per field.</summary>
    [JsonPropertyName("max_facet_values")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? MaxFacetValues { get; set; }

    /// <summary>Facet value query for filtering facet counts.</summary>
    [JsonPropertyName("facet_query")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? FacetQuery { get; set; }

    /// <summary>Typos a facet query word may have (default 2).</summary>
    [JsonPropertyName("facet_query_num_typos")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? FacetQueryNumTypos { get; set; }

    /// <summary>Comma-separated list of fields to highlight.</summary>
    [JsonPropertyName("highlight_fields")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? HighlightFields { get; set; }

    /// <summary>HTML tag to wrap the start of highlighted tokens.</summary>
    [JsonPropertyName("highlight_start_tag")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? HighlightStartTag { get; set; }

    /// <summary>HTML tag to wrap the end of highlighted tokens.</summary>
    [JsonPropertyName("highlight_end_tag")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? HighlightEndTag { get; set; }

    /// <summary>Number of tokens around each highlight for snippet context.</summary>
    [JsonPropertyName("highlight_affix_num_tokens")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? HighlightAffixNumTokens { get; set; }

    /// <summary>Have a model turn <see cref="Q"/> into search parameters.</summary>
    [JsonPropertyName("nl_query")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? NlQuery { get; set; }

    /// <summary>The natural-language model to use (default: the model named <c>default</c>).</summary>
    [JsonPropertyName("nl_model_id")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? NlModelId { get; set; }

    /// <summary>Also return the model's answer, in <c>parsed_nl_query.llm_response</c>.</summary>
    [JsonPropertyName("nl_query_debug")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public bool? NlQueryDebug { get; set; }

    /// <summary>Seconds the collection's schema prompt is kept (default 86400).</summary>
    [JsonPropertyName("nl_query_prompt_cache_ttl")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public int? NlQueryPromptCacheTtl { get; set; }

    /// <summary>This search's own API key (else the request's): a scoped key's embedded parameters apply to it alone.</summary>
    [JsonPropertyName("x-typesense-api-key")]
    [JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)]
    public string? ApiKey { get; set; }
}

/// <summary>
/// Response from a multi-collection search containing results for each query.
/// </summary>
public sealed class MultiSearchResponse
{
    /// <summary>Results for each search query, in the same order as the request.</summary>
    [JsonPropertyName("results")]
    public List<JsonElement> Results { get; set; } = [];

    /// <summary>The answer of a conversation over every search's results.</summary>
    [JsonPropertyName("conversation")]
    public ConversationAnswer? Conversation { get; set; }
}
