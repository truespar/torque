package com.truespar.torque.http;

import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Map;

/**
 * Typed, fluent builder for Typesense v30.1 search parameters.
 *
 * <p>Provides compile-time safety and IDE auto-completion for all search parameters,
 * replacing the error-prone {@code Map<String, Object>} approach.
 *
 * <p>Usage:
 * <pre>{@code
 * var params = new SearchParameters()
 *     .q("laptop")
 *     .queryBy("title,description")
 *     .filterBy("price:>100 && brand:=Apple")
 *     .sortBy("price:asc")
 *     .perPage(20);
 *
 * SearchResult result = client.searchTyped("products", params);
 * }</pre>
 *
 * @see TorqueHttpClient#searchTyped(String, SearchParameters)
 * @see TorqueHttpClient#search(String, SearchParameters)
 */
public final class SearchParameters {

    // ── Core search ────────────────────────────────────────────────────

    private String q;
    private String queryBy;
    private String queryByWeights;
    private String filterBy;
    private String sortBy;
    private String facetBy;
    private String groupBy;
    private Integer groupLimit;

    // ── Pagination ─────────────────────────────────────────────────────

    private Integer perPage;
    private Integer page;
    private Integer offset;
    private Integer limit;

    // ── Search behavior ────────────────────────────────────────────────

    private Boolean prefix;
    private Integer numTypos;

    // ── Field projection ───────────────────────────────────────────────

    private String includeFields;
    private String excludeFields;
    private String highlightFields;
    private String highlightFullFields;

    // ── Overrides and curation ─────────────────────────────────────────

    private String pinnedHits;
    private String hiddenHits;
    private Boolean enableOverrides;

    // ── Faceting ───────────────────────────────────────────────────────

    private Integer maxFacetValues;
    private String facetQuery;

    // ── Exhaustive search ──────────────────────────────────────────────

    private Boolean exhaustiveSearch;

    // ── Typo tolerance ─────────────────────────────────────────────────

    private Integer typoTokensThreshold;
    private Integer dropTokensThreshold;
    private String dropTokensMode;
    private Integer minLen1Typo;
    private Integer minLen2Typo;
    private String splitJoinTokens;
    private Integer synonymNumTypos;
    private Boolean enableTyposForNumericalTokens;
    private Boolean enableTyposForAlphaNumericalTokens;

    // ── Infix search ───────────────────────────────────────────────────

    private String infix;
    private Integer maxExtraPrefix;
    private Integer maxExtraSuffix;

    // ── Highlighting (advanced) ────────────────────────────────────────

    private String highlightStartTag;
    private String highlightEndTag;
    private Integer highlightAffixNumTokens;
    private Integer snippetThreshold;

    // ── Faceting (advanced) ────────────────────────────────────────────

    private String facetStrategy;
    private Integer facetSamplePercent;
    private Integer facetSampleThreshold;
    private String facetReturnParent;

    // ── Grouping (advanced) ────────────────────────────────────────────

    private Boolean groupMissingValues;

    // ── Performance & caching ──────────────────────────────────────────

    private Integer searchCutoffMs;
    private Integer maxCandidates;
    private Boolean useCache;
    private Integer cacheTtl;

    // ── Toggles ────────────────────────────────────────────────────────

    private Boolean validateFieldNames;
    private Boolean enableSynonyms;

    // ── Vector, ranking, presets, stopwords ────────────────────────────

    private String vectorQuery;
    private String preset;
    private String stopwords;
    private String synonymSets;
    private String textMatchType;
    private Boolean prioritizeExactMatch;

    // ── Fluent setters ─────────────────────────────────────────────────

    /**
     * Sets the search query text.
     *
     * @param q the query string; use {@code "*"} to match all documents
     * @return this builder
     */
    public SearchParameters q(String q) { this.q = q; return this; }

    /**
     * Sets the fields to search against, as a comma-separated list.
     *
     * @param queryBy comma-separated field names (e.g. {@code "title,description"})
     * @return this builder
     */
    public SearchParameters queryBy(String queryBy) { this.queryBy = queryBy; return this; }

    /**
     * Sets relative weights for fields in {@code query_by}, as a comma-separated list.
     *
     * @param queryByWeights comma-separated integers (e.g. {@code "3,1"})
     * @return this builder
     */
    public SearchParameters queryByWeights(String queryByWeights) { this.queryByWeights = queryByWeights; return this; }

    /**
     * Sets the filter expression for narrowing results.
     *
     * @param filterBy Typesense filter syntax (e.g. {@code "price:>100 && brand:=Apple"})
     * @return this builder
     */
    public SearchParameters filterBy(String filterBy) { this.filterBy = filterBy; return this; }

    /**
     * Sets the sort order for results.
     *
     * @param sortBy comma-separated sort clauses (e.g. {@code "price:asc,_text_match:desc"})
     * @return this builder
     */
    public SearchParameters sortBy(String sortBy) { this.sortBy = sortBy; return this; }

    /**
     * Sets the fields to facet on, as a comma-separated list.
     *
     * @param facetBy comma-separated field names (e.g. {@code "brand,category"})
     * @return this builder
     */
    public SearchParameters facetBy(String facetBy) { this.facetBy = facetBy; return this; }

    /**
     * Sets the field to group results by.
     *
     * @param groupBy field name to group on
     * @return this builder
     */
    public SearchParameters groupBy(String groupBy) { this.groupBy = groupBy; return this; }

    /**
     * Sets the maximum number of hits within each group.
     *
     * @param groupLimit max hits per group (default: 3)
     * @return this builder
     */
    public SearchParameters groupLimit(int groupLimit) { this.groupLimit = groupLimit; return this; }

    /**
     * Sets the number of results per page.
     *
     * @param perPage results per page (default: 10, max: 250)
     * @return this builder
     */
    public SearchParameters perPage(int perPage) { this.perPage = perPage; return this; }

    /**
     * Sets the page number (1-indexed).
     *
     * @param page page number starting from 1
     * @return this builder
     */
    public SearchParameters page(int page) { this.page = page; return this; }

    /**
     * Sets the offset for cursor-based pagination.
     *
     * @param offset zero-based offset into the result set
     * @return this builder
     */
    public SearchParameters offset(int offset) { this.offset = offset; return this; }

    /**
     * Sets the total number of results to return (cursor-based pagination).
     *
     * @param limit maximum number of results
     * @return this builder
     */
    public SearchParameters limit(int limit) { this.limit = limit; return this; }

    /**
     * Enables or disables prefix matching on the last query token.
     *
     * @param prefix {@code true} to enable prefix search (default), {@code false} to disable
     * @return this builder
     */
    public SearchParameters prefix(boolean prefix) { this.prefix = prefix; return this; }

    /**
     * Sets the maximum number of typographical errors allowed per query token.
     *
     * @param numTypos 0, 1, or 2 (default: 2)
     * @return this builder
     */
    public SearchParameters numTypos(int numTypos) { this.numTypos = numTypos; return this; }

    /**
     * Sets the fields to include in the response documents.
     *
     * @param includeFields comma-separated field names
     * @return this builder
     */
    public SearchParameters includeFields(String includeFields) { this.includeFields = includeFields; return this; }

    /**
     * Sets the fields to exclude from the response documents.
     *
     * @param excludeFields comma-separated field names
     * @return this builder
     */
    public SearchParameters excludeFields(String excludeFields) { this.excludeFields = excludeFields; return this; }

    /**
     * Sets the fields to highlight in search results.
     *
     * @param highlightFields comma-separated field names
     * @return this builder
     */
    public SearchParameters highlightFields(String highlightFields) { this.highlightFields = highlightFields; return this; }

    /**
     * Sets the fields for which full highlight snippets are returned.
     *
     * @param highlightFullFields comma-separated field names
     * @return this builder
     */
    public SearchParameters highlightFullFields(String highlightFullFields) { this.highlightFullFields = highlightFullFields; return this; }

    /**
     * Sets document IDs to pin to specific positions in the result list.
     *
     * @param pinnedHits comma-separated {@code id:position} pairs (e.g. {@code "abc:1,def:2"})
     * @return this builder
     */
    public SearchParameters pinnedHits(String pinnedHits) { this.pinnedHits = pinnedHits; return this; }

    /**
     * Sets document IDs to hide from the search results.
     *
     * @param hiddenHits comma-separated document IDs
     * @return this builder
     */
    public SearchParameters hiddenHits(String hiddenHits) { this.hiddenHits = hiddenHits; return this; }

    /**
     * Enables or disables search overrides (curations) for this query.
     *
     * @param enableOverrides {@code true} to apply overrides (default), {@code false} to skip
     * @return this builder
     */
    public SearchParameters enableOverrides(boolean enableOverrides) { this.enableOverrides = enableOverrides; return this; }

    /**
     * Sets the maximum number of facet values to return per faceted field.
     *
     * @param maxFacetValues max facet values (default: 10)
     * @return this builder
     */
    public SearchParameters maxFacetValues(int maxFacetValues) { this.maxFacetValues = maxFacetValues; return this; }

    /**
     * Sets a filter query for refining facet values.
     *
     * @param facetQuery facet query string (e.g. {@code "brand:App"})
     * @return this builder
     */
    public SearchParameters facetQuery(String facetQuery) { this.facetQuery = facetQuery; return this; }

    /**
     * Enables or disables exhaustive search (bypasses WAND optimization).
     *
     * @param exhaustiveSearch {@code true} to scan all documents, {@code false} for optimized top-K
     * @return this builder
     */
    public SearchParameters exhaustiveSearch(boolean exhaustiveSearch) { this.exhaustiveSearch = exhaustiveSearch; return this; }

    /**
     * Sets the result-count threshold below which typo-corrected matches are added.
     *
     * @param typoTokensThreshold minimum result count before typo variants kick in (default: 1)
     * @return this builder
     */
    public SearchParameters typoTokensThreshold(int typoTokensThreshold) { this.typoTokensThreshold = typoTokensThreshold; return this; }

    /**
     * Sets the result-count threshold below which query tokens are dropped to widen results.
     *
     * @param dropTokensThreshold minimum result count before tokens are dropped (default: 1)
     * @return this builder
     */
    public SearchParameters dropTokensThreshold(int dropTokensThreshold) { this.dropTokensThreshold = dropTokensThreshold; return this; }

    /**
     * Sets the token drop direction.
     *
     * @param dropTokensMode {@code "right_to_left"}, {@code "left_to_right"}, or {@code "both_sides:3"}
     * @return this builder
     */
    public SearchParameters dropTokensMode(String dropTokensMode) { this.dropTokensMode = dropTokensMode; return this; }

    /**
     * Sets the minimum word length for 1-character typo tolerance.
     *
     * @param minLen1Typo minimum word length (default: 4)
     * @return this builder
     */
    public SearchParameters minLen1Typo(int minLen1Typo) { this.minLen1Typo = minLen1Typo; return this; }

    /**
     * Sets the minimum word length for 2-character typo tolerance.
     *
     * @param minLen2Typo minimum word length (default: 7)
     * @return this builder
     */
    public SearchParameters minLen2Typo(int minLen2Typo) { this.minLen2Typo = minLen2Typo; return this; }

    /**
     * Sets the split/join token behavior.
     *
     * @param splitJoinTokens {@code "off"}, {@code "fallback"}, or {@code "always"}
     * @return this builder
     */
    public SearchParameters splitJoinTokens(String splitJoinTokens) { this.splitJoinTokens = splitJoinTokens; return this; }

    /**
     * Sets the maximum number of typos allowed when matching synonyms.
     *
     * @param synonymNumTypos max typos for synonym matching (default: 0)
     * @return this builder
     */
    public SearchParameters synonymNumTypos(int synonymNumTypos) { this.synonymNumTypos = synonymNumTypos; return this; }

    /**
     * Sets whether typo tolerance applies to purely numerical tokens.
     *
     * @param enabled {@code true} to allow typos on numerical tokens (default)
     * @return this builder
     */
    public SearchParameters enableTyposForNumericalTokens(boolean enabled) { this.enableTyposForNumericalTokens = enabled; return this; }

    /**
     * Sets whether typo tolerance applies to alphanumerical tokens.
     *
     * @param enabled {@code true} to allow typos on alphanumerical tokens (default)
     * @return this builder
     */
    public SearchParameters enableTyposForAlphaNumericalTokens(boolean enabled) { this.enableTyposForAlphaNumericalTokens = enabled; return this; }

    /**
     * Sets the infix search mode per query_by field.
     *
     * @param infix comma-separated modes: {@code "off"}, {@code "fallback"}, or {@code "always"}
     * @return this builder
     */
    public SearchParameters infix(String infix) { this.infix = infix; return this; }

    /**
     * Sets the maximum extra leading characters allowed in an infix match.
     *
     * @param maxExtraPrefix max extra prefix characters
     * @return this builder
     */
    public SearchParameters maxExtraPrefix(int maxExtraPrefix) { this.maxExtraPrefix = maxExtraPrefix; return this; }

    /**
     * Sets the maximum extra trailing characters allowed in an infix match.
     *
     * @param maxExtraSuffix max extra suffix characters
     * @return this builder
     */
    public SearchParameters maxExtraSuffix(int maxExtraSuffix) { this.maxExtraSuffix = maxExtraSuffix; return this; }

    /**
     * Sets the HTML tag wrapped around the start of highlighted tokens.
     *
     * @param highlightStartTag start tag (default: {@code "<mark>"})
     * @return this builder
     */
    public SearchParameters highlightStartTag(String highlightStartTag) { this.highlightStartTag = highlightStartTag; return this; }

    /**
     * Sets the HTML tag wrapped around the end of highlighted tokens.
     *
     * @param highlightEndTag end tag (default: {@code "</mark>"})
     * @return this builder
     */
    public SearchParameters highlightEndTag(String highlightEndTag) { this.highlightEndTag = highlightEndTag; return this; }

    /**
     * Sets the number of tokens of context around each highlight.
     *
     * @param highlightAffixNumTokens context token count (default: 4)
     * @return this builder
     */
    public SearchParameters highlightAffixNumTokens(int highlightAffixNumTokens) { this.highlightAffixNumTokens = highlightAffixNumTokens; return this; }

    /**
     * Sets the word-count threshold above which field values are snippeted.
     *
     * @param snippetThreshold word count threshold (default: 30)
     * @return this builder
     */
    public SearchParameters snippetThreshold(int snippetThreshold) { this.snippetThreshold = snippetThreshold; return this; }

    /**
     * Sets the facet computation strategy.
     *
     * @param facetStrategy {@code "automatic"}, {@code "exhaustive"}, or {@code "top_values"}
     * @return this builder
     */
    public SearchParameters facetStrategy(String facetStrategy) { this.facetStrategy = facetStrategy; return this; }

    /**
     * Sets the percentage of documents sampled for approximate facet counting.
     *
     * @param facetSamplePercent sample percentage (0-100)
     * @return this builder
     */
    public SearchParameters facetSamplePercent(int facetSamplePercent) { this.facetSamplePercent = facetSamplePercent; return this; }

    /**
     * Sets the minimum result count before facet sampling activates.
     *
     * @param facetSampleThreshold result count threshold
     * @return this builder
     */
    public SearchParameters facetSampleThreshold(int facetSampleThreshold) { this.facetSampleThreshold = facetSampleThreshold; return this; }

    /**
     * Sets the nested facet fields whose parent object should be returned.
     *
     * @param facetReturnParent comma-separated nested field names
     * @return this builder
     */
    public SearchParameters facetReturnParent(String facetReturnParent) { this.facetReturnParent = facetReturnParent; return this; }

    /**
     * Sets whether documents missing the group_by field form their own group.
     *
     * @param groupMissingValues {@code true} to group missing values (default)
     * @return this builder
     */
    public SearchParameters groupMissingValues(boolean groupMissingValues) { this.groupMissingValues = groupMissingValues; return this; }

    /**
     * Sets the best-effort scoring deadline in milliseconds.
     *
     * @param searchCutoffMs cutoff in milliseconds
     * @return this builder
     */
    public SearchParameters searchCutoffMs(int searchCutoffMs) { this.searchCutoffMs = searchCutoffMs; return this; }

    /**
     * Sets the maximum number of candidate tokens considered per query token.
     *
     * @param maxCandidates candidate cap (default: 4)
     * @return this builder
     */
    public SearchParameters maxCandidates(int maxCandidates) { this.maxCandidates = maxCandidates; return this; }

    /**
     * Enables or disables the server-side search result cache for this query.
     *
     * @param useCache {@code true} to serve from cache when possible
     * @return this builder
     */
    public SearchParameters useCache(boolean useCache) { this.useCache = useCache; return this; }

    /**
     * Sets the cache time-to-live for this query's results.
     *
     * @param cacheTtl TTL in seconds (default: 60)
     * @return this builder
     */
    public SearchParameters cacheTtl(int cacheTtl) { this.cacheTtl = cacheTtl; return this; }

    /**
     * Sets whether field names in query parameters are validated against the schema.
     *
     * @param validateFieldNames {@code true} to reject unknown field names
     * @return this builder
     */
    public SearchParameters validateFieldNames(boolean validateFieldNames) { this.validateFieldNames = validateFieldNames; return this; }

    /**
     * Enables or disables synonym expansion for this query.
     *
     * @param enableSynonyms {@code true} to apply synonyms (default)
     * @return this builder
     */
    public SearchParameters enableSynonyms(boolean enableSynonyms) { this.enableSynonyms = enableSynonyms; return this; }

    /**
     * Sets the vector search expression.
     *
     * @param vectorQuery e.g. {@code "embedding:([0.1, 0.2], k:10)"}
     * @return this builder
     */
    public SearchParameters vectorQuery(String vectorQuery) { this.vectorQuery = vectorQuery; return this; }

    /**
     * Applies a saved search preset by name.
     *
     * @param preset preset ID
     * @return this builder
     */
    public SearchParameters preset(String preset) { this.preset = preset; return this; }

    /**
     * Sets the stopword set(s) to apply to the query.
     *
     * @param stopwords comma-separated stopword set IDs
     * @return this builder
     */
    public SearchParameters stopwords(String stopwords) { this.stopwords = stopwords; return this; }

    /**
     * Sets the synonym set(s) to apply to the query.
     *
     * @param synonymSets comma-separated synonym set names
     * @return this builder
     */
    public SearchParameters synonymSets(String synonymSets) { this.synonymSets = synonymSets; return this; }

    /**
     * Sets the text match scoring mode.
     *
     * @param textMatchType {@code "max_score"} or {@code "max_weight"}
     * @return this builder
     */
    public SearchParameters textMatchType(String textMatchType) { this.textMatchType = textMatchType; return this; }

    /**
     * Sets whether exact matches rank above typo/prefix matches.
     *
     * @param prioritizeExactMatch {@code true} to boost exact matches (default)
     * @return this builder
     */
    public SearchParameters prioritizeExactMatch(boolean prioritizeExactMatch) { this.prioritizeExactMatch = prioritizeExactMatch; return this; }

    // ── Getters ────────────────────────────────────────────────────────

    /** Returns the query text, or {@code null} if not set. */
    public String q() { return q; }

    /** Returns the query_by fields, or {@code null} if not set. */
    public String queryBy() { return queryBy; }

    /** Returns the query_by_weights, or {@code null} if not set. */
    public String queryByWeights() { return queryByWeights; }

    /** Returns the filter_by expression, or {@code null} if not set. */
    public String filterBy() { return filterBy; }

    /** Returns the sort_by clause, or {@code null} if not set. */
    public String sortBy() { return sortBy; }

    /** Returns the facet_by fields, or {@code null} if not set. */
    public String facetBy() { return facetBy; }

    /** Returns the group_by field, or {@code null} if not set. */
    public String groupBy() { return groupBy; }

    /** Returns the group limit, or {@code null} if not set. */
    public Integer groupLimit() { return groupLimit; }

    /** Returns the per_page count, or {@code null} if not set. */
    public Integer perPage() { return perPage; }

    /** Returns the page number, or {@code null} if not set. */
    public Integer page() { return page; }

    /** Returns the offset, or {@code null} if not set. */
    public Integer offset() { return offset; }

    /** Returns the limit, or {@code null} if not set. */
    public Integer limit() { return limit; }

    /** Returns the prefix flag, or {@code null} if not set. */
    public Boolean prefix() { return prefix; }

    /** Returns the num_typos, or {@code null} if not set. */
    public Integer numTypos() { return numTypos; }

    /** Returns the include_fields, or {@code null} if not set. */
    public String includeFields() { return includeFields; }

    /** Returns the exclude_fields, or {@code null} if not set. */
    public String excludeFields() { return excludeFields; }

    /** Returns the highlight_fields, or {@code null} if not set. */
    public String highlightFields() { return highlightFields; }

    /** Returns the highlight_full_fields, or {@code null} if not set. */
    public String highlightFullFields() { return highlightFullFields; }

    /** Returns the pinned_hits, or {@code null} if not set. */
    public String pinnedHits() { return pinnedHits; }

    /** Returns the hidden_hits, or {@code null} if not set. */
    public String hiddenHits() { return hiddenHits; }

    /** Returns the enable_overrides flag, or {@code null} if not set. */
    public Boolean enableOverrides() { return enableOverrides; }

    /** Returns the max_facet_values, or {@code null} if not set. */
    public Integer maxFacetValues() { return maxFacetValues; }

    /** Returns the facet_query, or {@code null} if not set. */
    public String facetQuery() { return facetQuery; }

    /** Returns the exhaustive_search flag, or {@code null} if not set. */
    public Boolean exhaustiveSearch() { return exhaustiveSearch; }

    /** Returns the typo_tokens_threshold, or {@code null} if not set. */
    public Integer typoTokensThreshold() { return typoTokensThreshold; }

    /** Returns the drop_tokens_threshold, or {@code null} if not set. */
    public Integer dropTokensThreshold() { return dropTokensThreshold; }

    /** Returns the drop_tokens_mode, or {@code null} if not set. */
    public String dropTokensMode() { return dropTokensMode; }

    /** Returns the min_len_1typo, or {@code null} if not set. */
    public Integer minLen1Typo() { return minLen1Typo; }

    /** Returns the min_len_2typo, or {@code null} if not set. */
    public Integer minLen2Typo() { return minLen2Typo; }

    /** Returns the split_join_tokens mode, or {@code null} if not set. */
    public String splitJoinTokens() { return splitJoinTokens; }

    /** Returns the synonym_num_typos, or {@code null} if not set. */
    public Integer synonymNumTypos() { return synonymNumTypos; }

    /** Returns the enable_typos_for_numerical_tokens flag, or {@code null} if not set. */
    public Boolean enableTyposForNumericalTokens() { return enableTyposForNumericalTokens; }

    /** Returns the enable_typos_for_alpha_numerical_tokens flag, or {@code null} if not set. */
    public Boolean enableTyposForAlphaNumericalTokens() { return enableTyposForAlphaNumericalTokens; }

    /** Returns the infix mode, or {@code null} if not set. */
    public String infix() { return infix; }

    /** Returns the max_extra_prefix, or {@code null} if not set. */
    public Integer maxExtraPrefix() { return maxExtraPrefix; }

    /** Returns the max_extra_suffix, or {@code null} if not set. */
    public Integer maxExtraSuffix() { return maxExtraSuffix; }

    /** Returns the highlight_start_tag, or {@code null} if not set. */
    public String highlightStartTag() { return highlightStartTag; }

    /** Returns the highlight_end_tag, or {@code null} if not set. */
    public String highlightEndTag() { return highlightEndTag; }

    /** Returns the highlight_affix_num_tokens, or {@code null} if not set. */
    public Integer highlightAffixNumTokens() { return highlightAffixNumTokens; }

    /** Returns the snippet_threshold, or {@code null} if not set. */
    public Integer snippetThreshold() { return snippetThreshold; }

    /** Returns the facet_strategy, or {@code null} if not set. */
    public String facetStrategy() { return facetStrategy; }

    /** Returns the facet_sample_percent, or {@code null} if not set. */
    public Integer facetSamplePercent() { return facetSamplePercent; }

    /** Returns the facet_sample_threshold, or {@code null} if not set. */
    public Integer facetSampleThreshold() { return facetSampleThreshold; }

    /** Returns the facet_return_parent, or {@code null} if not set. */
    public String facetReturnParent() { return facetReturnParent; }

    /** Returns the group_missing_values flag, or {@code null} if not set. */
    public Boolean groupMissingValues() { return groupMissingValues; }

    /** Returns the search_cutoff_ms, or {@code null} if not set. */
    public Integer searchCutoffMs() { return searchCutoffMs; }

    /** Returns the max_candidates, or {@code null} if not set. */
    public Integer maxCandidates() { return maxCandidates; }

    /** Returns the use_cache flag, or {@code null} if not set. */
    public Boolean useCache() { return useCache; }

    /** Returns the cache_ttl, or {@code null} if not set. */
    public Integer cacheTtl() { return cacheTtl; }

    /** Returns the validate_field_names flag, or {@code null} if not set. */
    public Boolean validateFieldNames() { return validateFieldNames; }

    /** Returns the enable_synonyms flag, or {@code null} if not set. */
    public Boolean enableSynonyms() { return enableSynonyms; }

    /** Returns the vector_query, or {@code null} if not set. */
    public String vectorQuery() { return vectorQuery; }

    /** Returns the preset, or {@code null} if not set. */
    public String preset() { return preset; }

    /** Returns the stopwords, or {@code null} if not set. */
    public String stopwords() { return stopwords; }

    /** Returns the synonym_sets, or {@code null} if not set. */
    public String synonymSets() { return synonymSets; }

    /** Returns the text_match_type, or {@code null} if not set. */
    public String textMatchType() { return textMatchType; }

    /** Returns the prioritize_exact_match flag, or {@code null} if not set. */
    public Boolean prioritizeExactMatch() { return prioritizeExactMatch; }

    // ── Conversion ─────────────────────────────────────────────────────

    /**
     * Converts all non-null parameters to a {@code Map<String, Object>} with
     * Typesense wire-format keys (snake_case).
     *
     * @return parameter map suitable for {@link TorqueHttpClient#search(String, Map)}
     */
    public Map<String, Object> toMap() {
        var map = new LinkedHashMap<String, Object>();
        if (q != null) map.put("q", q);
        if (queryBy != null) map.put("query_by", queryBy);
        if (queryByWeights != null) map.put("query_by_weights", queryByWeights);
        if (filterBy != null) map.put("filter_by", filterBy);
        if (sortBy != null) map.put("sort_by", sortBy);
        if (facetBy != null) map.put("facet_by", facetBy);
        if (groupBy != null) map.put("group_by", groupBy);
        if (groupLimit != null) map.put("group_limit", groupLimit);
        if (perPage != null) map.put("per_page", perPage);
        if (page != null) map.put("page", page);
        if (offset != null) map.put("offset", offset);
        if (limit != null) map.put("limit", limit);
        if (prefix != null) map.put("prefix", prefix);
        if (numTypos != null) map.put("num_typos", numTypos);
        if (includeFields != null) map.put("include_fields", includeFields);
        if (excludeFields != null) map.put("exclude_fields", excludeFields);
        if (highlightFields != null) map.put("highlight_fields", highlightFields);
        if (highlightFullFields != null) map.put("highlight_full_fields", highlightFullFields);
        if (pinnedHits != null) map.put("pinned_hits", pinnedHits);
        if (hiddenHits != null) map.put("hidden_hits", hiddenHits);
        if (enableOverrides != null) map.put("enable_overrides", enableOverrides);
        if (maxFacetValues != null) map.put("max_facet_values", maxFacetValues);
        if (facetQuery != null) map.put("facet_query", facetQuery);
        if (exhaustiveSearch != null) map.put("exhaustive_search", exhaustiveSearch);
        if (typoTokensThreshold != null) map.put("typo_tokens_threshold", typoTokensThreshold);
        if (dropTokensThreshold != null) map.put("drop_tokens_threshold", dropTokensThreshold);
        if (dropTokensMode != null) map.put("drop_tokens_mode", dropTokensMode);
        if (minLen1Typo != null) map.put("min_len_1typo", minLen1Typo);
        if (minLen2Typo != null) map.put("min_len_2typo", minLen2Typo);
        if (splitJoinTokens != null) map.put("split_join_tokens", splitJoinTokens);
        if (synonymNumTypos != null) map.put("synonym_num_typos", synonymNumTypos);
        if (enableTyposForNumericalTokens != null) map.put("enable_typos_for_numerical_tokens", enableTyposForNumericalTokens);
        if (enableTyposForAlphaNumericalTokens != null) map.put("enable_typos_for_alpha_numerical_tokens", enableTyposForAlphaNumericalTokens);
        if (infix != null) map.put("infix", infix);
        if (maxExtraPrefix != null) map.put("max_extra_prefix", maxExtraPrefix);
        if (maxExtraSuffix != null) map.put("max_extra_suffix", maxExtraSuffix);
        if (highlightStartTag != null) map.put("highlight_start_tag", highlightStartTag);
        if (highlightEndTag != null) map.put("highlight_end_tag", highlightEndTag);
        if (highlightAffixNumTokens != null) map.put("highlight_affix_num_tokens", highlightAffixNumTokens);
        if (snippetThreshold != null) map.put("snippet_threshold", snippetThreshold);
        if (facetStrategy != null) map.put("facet_strategy", facetStrategy);
        if (facetSamplePercent != null) map.put("facet_sample_percent", facetSamplePercent);
        if (facetSampleThreshold != null) map.put("facet_sample_threshold", facetSampleThreshold);
        if (facetReturnParent != null) map.put("facet_return_parent", facetReturnParent);
        if (groupMissingValues != null) map.put("group_missing_values", groupMissingValues);
        if (searchCutoffMs != null) map.put("search_cutoff_ms", searchCutoffMs);
        if (maxCandidates != null) map.put("max_candidates", maxCandidates);
        if (useCache != null) map.put("use_cache", useCache);
        if (cacheTtl != null) map.put("cache_ttl", cacheTtl);
        if (validateFieldNames != null) map.put("validate_field_names", validateFieldNames);
        if (enableSynonyms != null) map.put("enable_synonyms", enableSynonyms);
        if (vectorQuery != null) map.put("vector_query", vectorQuery);
        if (preset != null) map.put("preset", preset);
        if (stopwords != null) map.put("stopwords", stopwords);
        if (synonymSets != null) map.put("synonym_sets", synonymSets);
        if (textMatchType != null) map.put("text_match_type", textMatchType);
        if (prioritizeExactMatch != null) map.put("prioritize_exact_match", prioritizeExactMatch);
        return map;
    }

    /**
     * Converts all non-null parameters to a URL-encoded query string.
     *
     * <p>Example output: {@code "q=laptop&query_by=title&filter_by=price%3A%3E100"}
     *
     * @return URL-encoded query string (without leading {@code ?})
     */
    public String toQueryString() {
        var sb = new StringBuilder();
        for (var entry : toMap().entrySet()) {
            if (!sb.isEmpty()) sb.append('&');
            sb.append(encode(entry.getKey()))
              .append('=')
              .append(encode(String.valueOf(entry.getValue())));
        }
        return sb.toString();
    }

    private static String encode(String s) {
        return URLEncoder.encode(s, StandardCharsets.UTF_8).replace("+", "%20");
    }
}
