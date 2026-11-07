using System.Text;

namespace Torque.Http.Search;

/// <summary>
/// Search query parameters matching the Typesense v30.1 search API.
/// Use <see cref="SearchBuilder"/> for a fluent construction API.
/// </summary>
public sealed class SearchParameters
{
    // --- Query ---

    /// <summary>The search query text. Use <c>"*"</c> for a wildcard query matching all documents.</summary>
    public string? Q { get; set; }

    /// <summary>Comma-separated list of fields to search (e.g. <c>"title,description"</c>).</summary>
    public string? QueryBy { get; set; }

    /// <summary>Comma-separated weights for each query_by field (e.g. <c>"3,1"</c>).</summary>
    public string? QueryByWeights { get; set; }

    /// <summary>Enable prefix matching on the query terms.</summary>
    public bool? Prefix { get; set; }

    /// <summary>Text match mode: <c>"max_score"</c> or <c>"max_weight"</c>.</summary>
    public string? TextMatchType { get; set; }

    // --- Filter ---

    /// <summary>Filter expression to narrow results (e.g. <c>"price:&gt;100 &amp;&amp; status:=active"</c>).</summary>
    public string? FilterBy { get; set; }

    // --- Sort ---

    /// <summary>Comma-separated sort expression (e.g. <c>"price:asc,_text_match:desc"</c>).</summary>
    public string? SortBy { get; set; }

    // --- Facet ---

    /// <summary>Comma-separated list of fields to facet on.</summary>
    public string? FacetBy { get; set; }

    /// <summary>Maximum number of facet values to return per field.</summary>
    public int? MaxFacetValues { get; set; }

    /// <summary>Facet value query for filtering facet counts (e.g. <c>"category:sho"</c>).</summary>
    public string? FacetQuery { get; set; }

    /// <summary>Typos a facet query word may have (default 2).</summary>
    public int? FacetQueryNumTypos { get; set; }

    // --- Pagination ---

    /// <summary>Number of results per page (default 10, max 250).</summary>
    public int? PerPage { get; set; }

    /// <summary>Page number for paginated results (1-based).</summary>
    public int? Page { get; set; }

    /// <summary>Starting offset for results (alternative to page-based pagination).</summary>
    public int? Offset { get; set; }

    /// <summary>Maximum number of results to return when using offset-based pagination.</summary>
    public int? Limit { get; set; }

    /// <summary>Maximum total hits to consider for the query.</summary>
    public int? LimitHits { get; set; }

    // --- Grouping ---

    /// <summary>Field name to group results by.</summary>
    public string? GroupBy { get; set; }

    /// <summary>Maximum number of hits returned per group (default 3).</summary>
    public int? GroupLimit { get; set; }

    /// <summary>Whether to create a group for documents missing the group_by field.</summary>
    public bool? GroupMissingValues { get; set; }

    // --- Highlighting ---

    /// <summary>Comma-separated list of fields to highlight in results.</summary>
    public string? HighlightFields { get; set; }

    /// <summary>Comma-separated list of fields to return full highlighted values for.</summary>
    public string? HighlightFullFields { get; set; }

    /// <summary>HTML tag to wrap the start of highlighted tokens (default <c>"&lt;mark&gt;"</c>).</summary>
    public string? HighlightStartTag { get; set; }

    /// <summary>HTML tag to wrap the end of highlighted tokens (default <c>"&lt;/mark&gt;"</c>).</summary>
    public string? HighlightEndTag { get; set; }

    /// <summary>Number of tokens around each highlight for snippet context.</summary>
    public int? HighlightAffixNumTokens { get; set; }

    /// <summary>Minimum word count for field values to be returned as snippets instead of full values.</summary>
    public int? SnippetThreshold { get; set; }

    // --- Field selection ---

    /// <summary>Comma-separated list of document fields to include in results.</summary>
    public string? IncludeFields { get; set; }

    /// <summary>Comma-separated list of document fields to exclude from results.</summary>
    public string? ExcludeFields { get; set; }

    // --- Typo tolerance ---

    /// <summary>Maximum number of typos allowed per token (0, 1, or 2).</summary>
    public int? NumTypos { get; set; }

    /// <summary>Minimum token count threshold before typo tolerance is applied.</summary>
    public int? TypoTokensThreshold { get; set; }

    /// <summary>Minimum token count threshold before tokens are dropped from the query.</summary>
    public int? DropTokensThreshold { get; set; }

    /// <summary>Token drop strategy: <c>"right_to_left"</c>, <c>"left_to_right"</c>, or <c>"both_sides:3"</c>.</summary>
    public string? DropTokensMode { get; set; }

    /// <summary>Whether exact matches are ranked above typo-corrected matches.</summary>
    public bool? PrioritizeExactMatch { get; set; }

    /// <summary>Whether earlier token positions in the document are ranked higher.</summary>
    public bool? PrioritizeTokenPosition { get; set; }

    /// <summary>Whether documents matching more query_by fields are ranked higher.</summary>
    public bool? PrioritizeNumMatchingFields { get; set; }

    // --- Typo advanced ---

    /// <summary>Minimum word length to consider 1-character typos (default: 4).</summary>
    public int? MinLen1Typo { get; set; }

    /// <summary>Minimum word length to consider 2-character typos (default: 8).</summary>
    public int? MinLen2Typo { get; set; }

    /// <summary>Controls split/join behavior of tokens: <c>"off"</c>, <c>"fallback"</c>, or <c>"always"</c>.</summary>
    public string? SplitJoinTokens { get; set; }

    /// <summary>Maximum number of typos allowed when matching synonyms (default: 0).</summary>
    public int? SynonymNumTypos { get; set; }

    /// <summary>Whether typo tolerance applies to purely numerical tokens (default: true).</summary>
    public bool? EnableTyposForNumericalTokens { get; set; }

    /// <summary>Whether typo tolerance applies to alphanumerical tokens (default: true).</summary>
    public bool? EnableTyposForAlphaNumericalTokens { get; set; }

    // --- Infix ---

    /// <summary>Comma-separated infix search mode per query_by field: <c>"off"</c>, <c>"always"</c>, or <c>"fallback"</c>.</summary>
    public string? Infix { get; set; }

    /// <summary>Maximum number of extra leading characters allowed in an infix match.</summary>
    public int? MaxExtraPrefix { get; set; }

    /// <summary>Maximum number of extra trailing characters allowed in an infix match.</summary>
    public int? MaxExtraSuffix { get; set; }

    // --- Name matching (Torque extension, fields with name_matching) ---

    /// <summary>Comma-separated per query_by field: <c>"off"</c>, <c>"fallback"</c> (the default) or <c>"always"</c>:
    /// when a name-matching field's words are also found by spelling variants and sound.</summary>
    public string? NameMatching { get; set; }

    /// <summary>Score the first <see cref="NameScoreCandidates"/> hits by name and order them by it (each hit's
    /// name_score).</summary>
    public bool? NameScore { get; set; }

    /// <summary>How many hits are scored by name (default 100, at most 1000); pages come from them.</summary>
    public int? NameScoreCandidates { get; set; }

    /// <summary><c>"person"</c> or <c>"organization"</c>: what the query names. One part of a person's name
    /// identifies no one; a company's one distinctive word does.</summary>
    public string? NameScoreKind { get; set; }

    /// <summary>Report in each hit's text_match_info how each query word matched.</summary>
    public bool? ExplainMatches { get; set; }

    /// <summary>Whether the query has already been segmented into tokens by the client.</summary>
    public bool? PreSegmentedQuery { get; set; }

    // --- Filter advanced ---

    /// <summary>Whether to enable lazy evaluation of filter conditions.</summary>
    public bool? EnableLazyFilter { get; set; }

    /// <summary>Maximum number of candidates to evaluate during filter processing.</summary>
    public int? MaxFilterByCandidates { get; set; }

    // --- Facet advanced ---

    /// <summary>Facet computation strategy: <c>"automatic"</c>, <c>"exhaustive"</c>, or <c>"top_values"</c>.</summary>
    public string? FacetStrategy { get; set; }

    /// <summary>Percentage of documents to sample for approximate facet counting (0-100).</summary>
    public int? FacetSamplePercent { get; set; }

    /// <summary>Minimum result count before facet sampling activates.</summary>
    public int? FacetSampleThreshold { get; set; }

    /// <summary>Comma-separated list of nested facet fields whose parent object should be returned.</summary>
    public string? FacetReturnParent { get; set; }

    // --- Feature toggles ---

    /// <summary>Whether to apply override/curation rules to this query (default: true).</summary>
    public bool? EnableOverrides { get; set; }

    /// <summary>Whether to apply synonym expansion to this query (default: true).</summary>
    public bool? EnableSynonyms { get; set; }

    /// <summary>Whether to validate that field names in query_by, filter_by, etc. exist in the schema.</summary>
    public bool? ValidateFieldNames { get; set; }

    // --- Vector ---

    /// <summary>Vector search query (e.g. <c>"vec_field:([0.1, 0.2, ...], k:10)"</c>).</summary>
    public string? VectorQuery { get; set; }

    // --- Curation ---

    /// <summary>Comma-separated list of document IDs to pin at specific positions.</summary>
    public string? PinnedHits { get; set; }

    /// <summary>Comma-separated list of document IDs to exclude from results.</summary>
    public string? HiddenHits { get; set; }

    // --- Stopwords & Synonyms ---

    /// <summary>Comma-separated list of stopword set IDs to apply to the query.</summary>
    public string? Stopwords { get; set; }

    /// <summary>Comma-separated list of synonym set names to apply to the query.</summary>
    public string? SynonymSets { get; set; }

    // --- Presets ---

    /// <summary>Preset ID whose saved parameters are merged with this query.</summary>
    public string? Preset { get; set; }

    // --- Performance ---

    /// <summary>Maximum time in milliseconds to spend on the search before returning partial results.</summary>
    public int? SearchCutoffMs { get; set; }

    /// <summary>Whether to perform an exhaustive search instead of using approximation heuristics.</summary>
    public bool? ExhaustiveSearch { get; set; }

    /// <summary>Maximum number of candidates to consider during approximate search.</summary>
    public int? MaxCandidates { get; set; }

    // --- Natural language ---

    /// <summary>Have a model turn <see cref="Q"/> into search parameters (q, filter_by, sort_by); the response reports them in <c>parsed_nl_query</c>.</summary>
    public bool? NlQuery { get; set; }

    /// <summary>The natural-language model to use (default: the model named <c>default</c>).</summary>
    public string? NlModelId { get; set; }

    /// <summary>Also return the model's answer, in <c>parsed_nl_query.llm_response</c>.</summary>
    public bool? NlQueryDebug { get; set; }

    /// <summary>Seconds the collection's schema prompt is kept (default 86400).</summary>
    public int? NlQueryPromptCacheTtl { get; set; }

    // --- Conversation ---

    /// <summary>Answer <see cref="Q"/> as a question from the hits (conversational search); the answer is in <c>conversation</c>.</summary>
    public bool? Conversation { get; set; }

    /// <summary>The conversation model answering (required with <see cref="Conversation"/>).</summary>
    public string? ConversationModelId { get; set; }

    /// <summary>A conversation to continue: the question is asked with its history.</summary>
    public string? ConversationId { get; set; }

    // --- Caching ---

    /// <summary>Whether to serve this query from the search cache if available.</summary>
    public bool? UseCache { get; set; }

    /// <summary>Cache time-to-live in seconds for this query's results.</summary>
    public int? CacheTtl { get; set; }

    internal string ToQueryString()
    {
        var sb = new StringBuilder(256);
        Append(sb, "q", Q);
        Append(sb, "query_by", QueryBy);
        Append(sb, "query_by_weights", QueryByWeights);
        Append(sb, "filter_by", FilterBy);
        Append(sb, "sort_by", SortBy);
        Append(sb, "facet_by", FacetBy);

        AppendInt(sb, "per_page", PerPage);
        AppendInt(sb, "page", Page);
        AppendInt(sb, "offset", Offset);
        AppendInt(sb, "limit", Limit);
        AppendInt(sb, "limit_hits", LimitHits);

        Append(sb, "group_by", GroupBy);
        AppendInt(sb, "group_limit", GroupLimit);
        AppendBool(sb, "group_missing_values", GroupMissingValues);

        Append(sb, "highlight_fields", HighlightFields);
        Append(sb, "highlight_full_fields", HighlightFullFields);
        Append(sb, "highlight_start_tag", HighlightStartTag);
        Append(sb, "highlight_end_tag", HighlightEndTag);
        AppendInt(sb, "highlight_affix_num_tokens", HighlightAffixNumTokens);
        AppendInt(sb, "snippet_threshold", SnippetThreshold);

        Append(sb, "include_fields", IncludeFields);
        Append(sb, "exclude_fields", ExcludeFields);

        Append(sb, "text_match_type", TextMatchType);
        AppendInt(sb, "num_typos", NumTypos);
        AppendInt(sb, "typo_tokens_threshold", TypoTokensThreshold);
        AppendInt(sb, "drop_tokens_threshold", DropTokensThreshold);
        Append(sb, "drop_tokens_mode", DropTokensMode);
        AppendBool(sb, "prefix", Prefix);
        AppendBool(sb, "prioritize_exact_match", PrioritizeExactMatch);
        AppendBool(sb, "prioritize_token_position", PrioritizeTokenPosition);
        AppendBool(sb, "prioritize_num_matching_fields", PrioritizeNumMatchingFields);

        AppendInt(sb, "min_len_1typo", MinLen1Typo);
        AppendInt(sb, "min_len_2typo", MinLen2Typo);
        Append(sb, "split_join_tokens", SplitJoinTokens);
        AppendInt(sb, "synonym_num_typos", SynonymNumTypos);
        AppendBool(sb, "enable_typos_for_numerical_tokens", EnableTyposForNumericalTokens);
        AppendBool(sb, "enable_typos_for_alpha_numerical_tokens", EnableTyposForAlphaNumericalTokens);

        Append(sb, "infix", Infix);
        AppendInt(sb, "max_extra_prefix", MaxExtraPrefix);
        AppendInt(sb, "max_extra_suffix", MaxExtraSuffix);
        Append(sb, "name_matching", NameMatching);
        AppendBool(sb, "name_score", NameScore);
        AppendInt(sb, "name_score_candidates", NameScoreCandidates);
        Append(sb, "name_score_kind", NameScoreKind);
        AppendBool(sb, "explain_matches", ExplainMatches);
        AppendBool(sb, "pre_segmented_query", PreSegmentedQuery);

        AppendBool(sb, "enable_lazy_filter", EnableLazyFilter);
        AppendInt(sb, "max_filter_by_candidates", MaxFilterByCandidates);

        Append(sb, "facet_strategy", FacetStrategy);
        AppendInt(sb, "facet_sample_percent", FacetSamplePercent);
        AppendInt(sb, "facet_sample_threshold", FacetSampleThreshold);
        Append(sb, "facet_return_parent", FacetReturnParent);

        AppendBool(sb, "enable_overrides", EnableOverrides);
        AppendBool(sb, "enable_synonyms", EnableSynonyms);
        AppendBool(sb, "validate_field_names", ValidateFieldNames);

        Append(sb, "vector_query", VectorQuery);
        AppendInt(sb, "max_facet_values", MaxFacetValues);
        Append(sb, "facet_query", FacetQuery);
        AppendInt(sb, "facet_query_num_typos", FacetQueryNumTypos);

        Append(sb, "pinned_hits", PinnedHits);
        Append(sb, "hidden_hits", HiddenHits);

        Append(sb, "stopwords", Stopwords);
        Append(sb, "synonym_sets", SynonymSets);
        Append(sb, "preset", Preset);

        AppendInt(sb, "search_cutoff_ms", SearchCutoffMs);
        AppendBool(sb, "exhaustive_search", ExhaustiveSearch);
        AppendInt(sb, "max_candidates", MaxCandidates);

        AppendBool(sb, "use_cache", UseCache);
        AppendInt(sb, "cache_ttl", CacheTtl);

        AppendBool(sb, "nl_query", NlQuery);
        Append(sb, "nl_model_id", NlModelId);
        AppendBool(sb, "nl_query_debug", NlQueryDebug);
        AppendInt(sb, "nl_query_prompt_cache_ttl", NlQueryPromptCacheTtl);

        AppendBool(sb, "conversation", Conversation);
        Append(sb, "conversation_model_id", ConversationModelId);
        Append(sb, "conversation_id", ConversationId);

        return sb.ToString();
    }

    private static void Append(StringBuilder sb, string key, string? value)
    {
        if (value is null) return;
        sb.Append(sb.Length == 0 ? '?' : '&');
        sb.Append(Uri.EscapeDataString(key));
        sb.Append('=');
        sb.Append(Uri.EscapeDataString(value));
    }

    private static void AppendInt(StringBuilder sb, string key, int? value)
    {
        if (!value.HasValue) return;
        sb.Append(sb.Length == 0 ? '?' : '&');
        sb.Append(Uri.EscapeDataString(key));
        sb.Append('=');
        sb.Append(value.Value);
    }

    private static void AppendBool(StringBuilder sb, string key, bool? value)
    {
        if (!value.HasValue) return;
        sb.Append(sb.Length == 0 ? '?' : '&');
        sb.Append(Uri.EscapeDataString(key));
        sb.Append('=');
        sb.Append(value.Value ? "true" : "false");
    }
}
