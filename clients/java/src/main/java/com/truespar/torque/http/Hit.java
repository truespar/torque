package com.truespar.torque.http;

import java.util.List;
import java.util.Map;

/**
 * A single search hit returned by the Torque search API.
 *
 * <p>Contains the matched document, its text relevance score, and optional
 * highlight snippets showing where the query terms matched.
 *
 * @param document   the full document as a key-value map
 * @param textMatch  text relevance score (higher is more relevant); 0 if not applicable
 * @param highlights list of highlight objects, each containing field name and matched tokens
 *
 * @see SearchResult
 */
public record Hit(
        Map<String, Object> document,
        long textMatch,
        List<Map<String, Object>> highlights
) {

    /**
     * Creates a {@code Hit} from a raw API response map.
     *
     * <p>Extracts {@code document}, {@code text_match}, and {@code highlights} keys
     * from the map. Missing keys are replaced with safe defaults.
     *
     * @param raw the raw map from the JSON search response
     * @return a typed {@code Hit} instance
     */
    @SuppressWarnings("unchecked")
    public static Hit fromMap(Map<String, Object> raw) {
        var doc = raw.containsKey("document")
                ? (Map<String, Object>) raw.get("document")
                : Map.<String, Object>of();

        long score = 0;
        var textMatchVal = raw.get("text_match");
        if (textMatchVal instanceof Number n) {
            score = n.longValue();
        }

        var hl = raw.containsKey("highlights")
                ? (List<Map<String, Object>>) raw.get("highlights")
                : List.<Map<String, Object>>of();

        return new Hit(doc, score, hl);
    }
}
