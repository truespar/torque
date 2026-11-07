package com.truespar.torque.http;

import java.util.List;
import java.util.Map;

/**
 * Typed representation of a Torque/Typesense search response.
 *
 * <p>Provides strongly-typed access to common result fields instead of navigating
 * a raw {@code Map<String, Object>}. Created automatically by
 * {@link TorqueHttpClient#searchTyped(String, SearchParameters)}.
 *
 * <p>Usage:
 * <pre>{@code
 * SearchResult result = client.searchTyped("products",
 *     new SearchParameters().q("laptop").queryBy("title"));
 *
 * System.out.println("Found: " + result.found());
 * for (Hit hit : result.hits()) {
 *     System.out.println(hit.document().get("title"));
 * }
 * }</pre>
 *
 * @param found        total number of documents matching the query
 * @param outOf        total number of documents in the collection
 * @param hits         list of matched documents with scores and highlights
 * @param facetCounts  facet value counts (empty list if no facets requested)
 * @param searchTimeMs server-side search duration in milliseconds
 * @param page         the page number of this result set
 * @param rawResponse  the complete raw response map for accessing any additional fields
 *
 * @see Hit
 * @see SearchParameters
 */
public record SearchResult(
        int found,
        int outOf,
        List<Hit> hits,
        List<Map<String, Object>> facetCounts,
        int searchTimeMs,
        int page,
        Map<String, Object> rawResponse
) {

    /**
     * Creates a {@code SearchResult} from a raw API response map.
     *
     * <p>Extracts and converts typed fields from the JSON response. Any fields
     * not covered by this record are still accessible via {@link #rawResponse()}.
     *
     * @param raw the raw map from the JSON search response
     * @return a typed {@code SearchResult} instance
     */
    @SuppressWarnings("unchecked")
    public static SearchResult fromMap(Map<String, Object> raw) {
        int found = intVal(raw, "found");
        int outOf = intVal(raw, "out_of");
        int searchTime = intVal(raw, "search_time_ms");
        int page = intVal(raw, "page");

        var rawHits = raw.containsKey("hits")
                ? (List<Map<String, Object>>) raw.get("hits")
                : List.<Map<String, Object>>of();
        var hits = rawHits.stream().map(Hit::fromMap).toList();

        var facetCounts = raw.containsKey("facet_counts")
                ? (List<Map<String, Object>>) raw.get("facet_counts")
                : List.<Map<String, Object>>of();

        return new SearchResult(found, outOf, hits, facetCounts, searchTime, page, raw);
    }

    private static int intVal(Map<String, Object> map, String key) {
        var v = map.get(key);
        return v instanceof Number n ? n.intValue() : 0;
    }
}
