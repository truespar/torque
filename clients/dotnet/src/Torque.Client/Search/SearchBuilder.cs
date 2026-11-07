namespace Torque.Http.Search;

/// <summary>
/// Fluent builder for constructing <see cref="SearchParameters"/> with a clean, chainable API.
/// </summary>
/// <example>
/// <code>
/// var results = await client.SearchAsync("products",
///     SearchBuilder.Create("title,description")
///         .Query("running shoes")
///         .FilterBy("price:&gt;50 &amp;&amp; brand:=Nike")
///         .SortBy("price:asc")
///         .FacetBy("brand,category")
///         .Page(1, perPage: 20)
///         .HighlightFields("title", "description"));
/// </code>
/// </example>
public sealed class SearchBuilder
{
    private readonly SearchParameters _params = new();

    private SearchBuilder() { }

    /// <summary>
    /// Start building a search. <paramref name="queryBy"/> specifies the fields to search.
    /// </summary>
    public static SearchBuilder Create(string queryBy)
    {
        var builder = new SearchBuilder();
        builder._params.QueryBy = queryBy;
        return builder;
    }

    /// <summary>Sets the search query text. Use <c>"*"</c> for a wildcard query.</summary>
    /// <param name="q">The search query text.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder Query(string q) { _params.Q = q; return this; }

    /// <summary>Sets the filter expression to narrow results.</summary>
    /// <param name="filter">Filter expression (e.g. <c>"price:&gt;100 &amp;&amp; status:=active"</c>).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder FilterBy(string filter) { _params.FilterBy = filter; return this; }

    /// <summary>Sets the sort expression.</summary>
    /// <param name="sort">Comma-separated sort expression (e.g. <c>"price:asc,_text_match:desc"</c>).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder SortBy(string sort) { _params.SortBy = sort; return this; }

    /// <summary>Sets the fields to facet on.</summary>
    /// <param name="facet">Comma-separated list of fields to facet on.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder FacetBy(string facet) { _params.FacetBy = facet; return this; }

    /// <summary>Sets the field to group results by.</summary>
    /// <param name="groupBy">Field name to group results by.</param>
    /// <param name="limit">Maximum number of hits per group (default 3).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder GroupBy(string groupBy, int? limit = null)
    {
        _params.GroupBy = groupBy;
        _params.GroupLimit = limit;
        return this;
    }

    /// <summary>Whether to create a group for documents missing the group_by field.</summary>
    /// <param name="include">True to include documents missing the group field.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder GroupMissingValues(bool include = true) { _params.GroupMissingValues = include; return this; }

    /// <summary>Sets page-based pagination.</summary>
    /// <param name="page">Page number (1-based).</param>
    /// <param name="perPage">Number of results per page (default 10, max 250).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder Page(int page, int perPage = 10)
    {
        _params.Page = page;
        _params.PerPage = perPage;
        return this;
    }

    /// <summary>Sets offset-based pagination.</summary>
    /// <param name="offset">Starting offset for results.</param>
    /// <param name="limit">Maximum number of results to return.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder Offset(int offset, int limit)
    {
        _params.Offset = offset;
        _params.Limit = limit;
        return this;
    }

    /// <summary>Enables or disables prefix matching on query terms.</summary>
    /// <param name="enabled">True to enable prefix matching.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder Prefix(bool enabled = true) { _params.Prefix = enabled; return this; }

    /// <summary>Sets the maximum number of typos allowed per token.</summary>
    /// <param name="numTypos">Number of typos (0, 1, or 2).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder Typos(int numTypos) { _params.NumTypos = numTypos; return this; }

    /// <summary>Sets the minimum token count threshold before typo tolerance is applied.</summary>
    /// <param name="threshold">Token count threshold.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder TypoTokensThreshold(int threshold) { _params.TypoTokensThreshold = threshold; return this; }

    /// <summary>Sets the minimum token count threshold before tokens are dropped from the query.</summary>
    /// <param name="threshold">Token count threshold.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder DropTokensThreshold(int threshold) { _params.DropTokensThreshold = threshold; return this; }

    /// <summary>Sets the token drop strategy.</summary>
    /// <param name="mode">Drop mode: <c>"right_to_left"</c>, <c>"left_to_right"</c>, or <c>"both_sides:3"</c>.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder DropTokensMode(string mode) { _params.DropTokensMode = mode; return this; }

    /// <summary>Sets the document fields to include in results.</summary>
    /// <param name="fields">Field names to include.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder IncludeFields(params string[] fields) { _params.IncludeFields = string.Join(",", fields); return this; }

    /// <summary>Sets the document fields to exclude from results.</summary>
    /// <param name="fields">Field names to exclude.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder ExcludeFields(params string[] fields) { _params.ExcludeFields = string.Join(",", fields); return this; }

    /// <summary>Sets the fields to highlight in results.</summary>
    /// <param name="fields">Field names to highlight.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder HighlightFields(params string[] fields) { _params.HighlightFields = string.Join(",", fields); return this; }

    /// <summary>Sets the fields to return full highlighted values for.</summary>
    /// <param name="fields">Field names to fully highlight.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder HighlightFullFields(params string[] fields) { _params.HighlightFullFields = string.Join(",", fields); return this; }

    /// <summary>Sets the HTML tags wrapping highlighted tokens.</summary>
    /// <param name="start">Start tag (e.g. <c>"&lt;mark&gt;"</c>).</param>
    /// <param name="end">End tag (e.g. <c>"&lt;/mark&gt;"</c>).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder HighlightTags(string start, string end)
    {
        _params.HighlightStartTag = start;
        _params.HighlightEndTag = end;
        return this;
    }

    /// <summary>Sets the number of tokens around each highlight for snippet context.</summary>
    /// <param name="tokens">Number of context tokens.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder HighlightAffixNumTokens(int tokens) { _params.HighlightAffixNumTokens = tokens; return this; }

    /// <summary>Sets the minimum word count for field values to be returned as snippets.</summary>
    /// <param name="threshold">Snippet threshold.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder SnippetThreshold(int threshold) { _params.SnippetThreshold = threshold; return this; }

    /// <summary>Sets the text match mode.</summary>
    /// <param name="type">Match type: <c>"max_score"</c> or <c>"max_weight"</c>.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder TextMatchType(string type) { _params.TextMatchType = type; return this; }

    /// <summary>Sets weights for each query_by field.</summary>
    /// <param name="weights">Comma-separated weights (e.g. <c>"3,1"</c>).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder QueryByWeights(string weights) { _params.QueryByWeights = weights; return this; }

    /// <summary>Whether exact matches are ranked above typo-corrected matches.</summary>
    /// <param name="enabled">True to prioritize exact matches.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder PrioritizeExactMatch(bool enabled = true) { _params.PrioritizeExactMatch = enabled; return this; }

    /// <summary>Whether earlier token positions in the document are ranked higher.</summary>
    /// <param name="enabled">True to prioritize token position.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder PrioritizeTokenPosition(bool enabled = true) { _params.PrioritizeTokenPosition = enabled; return this; }

    /// <summary>Whether documents matching more query_by fields are ranked higher.</summary>
    /// <param name="enabled">True to prioritize number of matching fields.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder PrioritizeNumMatchingFields(bool enabled = true) { _params.PrioritizeNumMatchingFields = enabled; return this; }

    /// <summary>Sets a raw vector search query string.</summary>
    /// <param name="query">Vector query (e.g. <c>"vec_field:([0.1, 0.2, ...], k:10)"</c>).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder VectorQuery(string query) { _params.VectorQuery = query; return this; }

    /// <summary>Sets a vector search query from a field name, vector, and k.</summary>
    /// <param name="field">The vector field name.</param>
    /// <param name="vector">The query vector.</param>
    /// <param name="k">Number of nearest neighbors to return.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder VectorQuery(string field, float[] vector, int k)
    {
        var vecStr = string.Join(",", vector.Select(v => v.ToString("G")));
        _params.VectorQuery = $"{field}:([{vecStr}], k:{k})";
        return this;
    }

    /// <summary>Sets the maximum number of facet values to return per field.</summary>
    /// <param name="count">Maximum facet values per field.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder MaxFacetValues(int count) { _params.MaxFacetValues = count; return this; }

    /// <summary>Sets the facet value query for filtering facet counts.</summary>
    /// <param name="query">Facet query (e.g. <c>"category:sho"</c>).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder FacetQuery(string query) { _params.FacetQuery = query; return this; }

    /// <summary>Sets document IDs to pin at specific positions.</summary>
    /// <param name="pinnedHits">Document IDs to pin.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder PinnedHits(params string[] pinnedHits) { _params.PinnedHits = string.Join(",", pinnedHits); return this; }

    /// <summary>Sets document IDs to exclude from results.</summary>
    /// <param name="hiddenHits">Document IDs to hide.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder HiddenHits(params string[] hiddenHits) { _params.HiddenHits = string.Join(",", hiddenHits); return this; }

    /// <summary>Sets the stopword set ID to apply to the query.</summary>
    /// <param name="setId">Stopword set ID.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder Stopwords(string setId) { _params.Stopwords = setId; return this; }

    /// <summary>Sets the synonym set names to apply to the query.</summary>
    /// <param name="sets">Comma-separated synonym set names.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder SynonymSets(string sets) { _params.SynonymSets = sets; return this; }

    /// <summary>Sets the preset ID whose saved parameters are merged with this query.</summary>
    /// <param name="presetId">Preset ID.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder Preset(string presetId) { _params.Preset = presetId; return this; }

    /// <summary>Sets the maximum search time before returning partial results.</summary>
    /// <param name="ms">Cutoff time in milliseconds.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder SearchCutoffMs(int ms) { _params.SearchCutoffMs = ms; return this; }

    /// <summary>Sets the maximum total hits to consider for the query.</summary>
    /// <param name="limit">Maximum number of total hits.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder LimitHits(int limit) { _params.LimitHits = limit; return this; }

    /// <summary>Enables or disables exhaustive search instead of approximation heuristics.</summary>
    /// <param name="enabled">True to enable exhaustive search.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder ExhaustiveSearch(bool enabled = true) { _params.ExhaustiveSearch = enabled; return this; }

    /// <summary>Sets the maximum number of candidates for approximate search.</summary>
    /// <param name="count">Maximum candidates.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder MaxCandidates(int count) { _params.MaxCandidates = count; return this; }

    /// <summary>Enables or disables serving this query from the search cache.</summary>
    /// <param name="enabled">True to use cache.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder UseCache(bool enabled = true) { _params.UseCache = enabled; return this; }

    /// <summary>Sets the cache time-to-live for this query's results.</summary>
    /// <param name="seconds">Cache TTL in seconds.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder CacheTtl(int seconds) { _params.CacheTtl = seconds; return this; }

    /// <summary>Sets the minimum word length for 1-character typo tolerance.</summary>
    /// <param name="length">Minimum word length (default 4).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder MinLen1Typo(int length) { _params.MinLen1Typo = length; return this; }

    /// <summary>Sets the minimum word length for 2-character typo tolerance.</summary>
    /// <param name="length">Minimum word length (default 8).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder MinLen2Typo(int length) { _params.MinLen2Typo = length; return this; }

    /// <summary>Sets the split/join tokens behavior.</summary>
    /// <param name="mode">Mode: <c>"off"</c>, <c>"fallback"</c>, or <c>"always"</c>.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder SplitJoinTokens(string mode) { _params.SplitJoinTokens = mode; return this; }

    /// <summary>Sets the maximum number of typos allowed when matching synonyms.</summary>
    /// <param name="numTypos">Max typos for synonym matching (default 0).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder SynonymNumTypos(int numTypos) { _params.SynonymNumTypos = numTypos; return this; }

    /// <summary>Sets whether typo tolerance applies to purely numerical tokens.</summary>
    /// <param name="enabled">True to allow typos on numerical tokens (default).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder EnableTyposForNumericalTokens(bool enabled = true) { _params.EnableTyposForNumericalTokens = enabled; return this; }

    /// <summary>Sets whether typo tolerance applies to alphanumerical tokens.</summary>
    /// <param name="enabled">True to allow typos on alphanumerical tokens (default).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder EnableTyposForAlphaNumericalTokens(bool enabled = true) { _params.EnableTyposForAlphaNumericalTokens = enabled; return this; }

    /// <summary>Sets infix search mode per query_by field.</summary>
    /// <param name="mode">Comma-separated modes: <c>"off"</c>, <c>"always"</c>, or <c>"fallback"</c>.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder Infix(string mode) { _params.Infix = mode; return this; }

    /// <summary>Sets the maximum extra leading characters allowed in an infix match.</summary>
    /// <param name="count">Max extra prefix characters.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder MaxExtraPrefix(int count) { _params.MaxExtraPrefix = count; return this; }

    /// <summary>Sets the maximum extra trailing characters allowed in an infix match.</summary>
    /// <param name="count">Max extra suffix characters.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder MaxExtraSuffix(int count) { _params.MaxExtraSuffix = count; return this; }

    /// <summary>Sets whether the query has been pre-segmented into tokens.</summary>
    /// <param name="enabled">True if the query is pre-segmented.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder PreSegmentedQuery(bool enabled = true) { _params.PreSegmentedQuery = enabled; return this; }

    /// <summary>Enables or disables lazy evaluation of filter conditions.</summary>
    /// <param name="enabled">True to enable lazy filter evaluation.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder EnableLazyFilter(bool enabled = true) { _params.EnableLazyFilter = enabled; return this; }

    /// <summary>Sets the maximum number of candidates to evaluate during filtering.</summary>
    /// <param name="count">Maximum filter candidates.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder MaxFilterByCandidates(int count) { _params.MaxFilterByCandidates = count; return this; }

    /// <summary>Sets the facet computation strategy.</summary>
    /// <param name="strategy">Strategy: <c>"automatic"</c>, <c>"exhaustive"</c>, or <c>"top_values"</c>.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder FacetStrategy(string strategy) { _params.FacetStrategy = strategy; return this; }

    /// <summary>Sets the percentage of documents to sample for approximate faceting.</summary>
    /// <param name="percent">Sample percentage (0-100).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder FacetSamplePercent(int percent) { _params.FacetSamplePercent = percent; return this; }

    /// <summary>Sets the minimum result count before facet sampling activates.</summary>
    /// <param name="threshold">Result count threshold.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder FacetSampleThreshold(int threshold) { _params.FacetSampleThreshold = threshold; return this; }

    /// <summary>Sets the nested facet fields whose parent object should be returned.</summary>
    /// <param name="fields">Nested facet field names.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder FacetReturnParent(params string[] fields) { _params.FacetReturnParent = string.Join(",", fields); return this; }

    /// <summary>Enables or disables override/curation rules for this query.</summary>
    /// <param name="enabled">True to enable overrides (default).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder EnableOverrides(bool enabled = true) { _params.EnableOverrides = enabled; return this; }

    /// <summary>Enables or disables synonym expansion for this query.</summary>
    /// <param name="enabled">True to enable synonyms (default).</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder EnableSynonyms(bool enabled = true) { _params.EnableSynonyms = enabled; return this; }

    /// <summary>Enables or disables field name validation in query parameters.</summary>
    /// <param name="enabled">True to validate field names.</param>
    /// <returns>This builder for chaining.</returns>
    public SearchBuilder ValidateFieldNames(bool enabled = true) { _params.ValidateFieldNames = enabled; return this; }

    /// <summary>
    /// Build the final <see cref="SearchParameters"/>.
    /// </summary>
    public SearchParameters Build() => _params;
}
