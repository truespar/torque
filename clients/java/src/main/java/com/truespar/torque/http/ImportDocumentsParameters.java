package com.truespar.torque.http;

import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;

/**
 * Parameters for the document import endpoint.
 *
 * <p>Controls import behavior including action type, dirty value handling,
 * batching, and response fields.
 *
 * <p>Usage:
 * <pre>{@code
 * var params = new ImportDocumentsParameters()
 *     .action("upsert")
 *     .batchSize(1000)
 *     .returnId(true);
 *
 * String result = client.importDocuments("products", jsonl, params);
 * }</pre>
 *
 * <p>For emplace (create-or-update without requiring all fields):
 * <pre>{@code
 * var params = new ImportDocumentsParameters().action("emplace");
 * }</pre>
 *
 * @see TorqueHttpClient#importDocuments(String, String, ImportDocumentsParameters)
 */
public final class ImportDocumentsParameters {

    private String action = "create";
    private String dirtyValues;
    private Integer batchSize;
    private Boolean returnDoc;
    private Boolean returnId;

    /**
     * Sets the import action.
     *
     * @param action one of {@code "create"}, {@code "upsert"}, {@code "update"}, or {@code "emplace"}
     * @return this builder
     */
    public ImportDocumentsParameters action(String action) { this.action = action; return this; }

    /**
     * Sets how dirty (type-mismatched) values are handled during import.
     *
     * @param dirtyValues one of {@code "coerce_or_reject"}, {@code "coerce_or_drop"}, {@code "drop"}, {@code "reject"}
     * @return this builder
     */
    public ImportDocumentsParameters dirtyValues(String dirtyValues) { this.dirtyValues = dirtyValues; return this; }

    /**
     * Sets the number of documents to process per internal batch.
     *
     * @param batchSize batch size (default: server-determined)
     * @return this builder
     */
    public ImportDocumentsParameters batchSize(int batchSize) { this.batchSize = batchSize; return this; }

    /**
     * Whether to include the full document in each import result line.
     *
     * @param returnDoc {@code true} to include the document
     * @return this builder
     */
    public ImportDocumentsParameters returnDoc(boolean returnDoc) { this.returnDoc = returnDoc; return this; }

    /**
     * Whether to include the document ID in each import result line.
     *
     * @param returnId {@code true} to include the ID
     * @return this builder
     */
    public ImportDocumentsParameters returnId(boolean returnId) { this.returnId = returnId; return this; }

    /** Returns the import action. */
    public String action() { return action; }

    /** Returns the dirty values handling mode, or {@code null} if not set. */
    public String dirtyValues() { return dirtyValues; }

    /** Returns the batch size, or {@code null} if not set. */
    public Integer batchSize() { return batchSize; }

    /** Returns the return_doc flag, or {@code null} if not set. */
    public Boolean returnDoc() { return returnDoc; }

    /** Returns the return_id flag, or {@code null} if not set. */
    public Boolean returnId() { return returnId; }

    /**
     * Converts the parameters to a URL query string fragment.
     *
     * @return URL-encoded query string (without leading {@code ?})
     */
    public String toQueryString() {
        var sb = new StringBuilder();
        append(sb, "action", action);
        if (dirtyValues != null) append(sb, "dirty_values", dirtyValues);
        if (batchSize != null) append(sb, "batch_size", String.valueOf(batchSize));
        if (returnDoc != null) append(sb, "return_doc", String.valueOf(returnDoc));
        if (returnId != null) append(sb, "return_id", String.valueOf(returnId));
        return sb.toString();
    }

    private static void append(StringBuilder sb, String key, String value) {
        if (!sb.isEmpty()) sb.append('&');
        sb.append(URLEncoder.encode(key, StandardCharsets.UTF_8))
          .append('=')
          .append(URLEncoder.encode(value, StandardCharsets.UTF_8).replace("+", "%20"));
    }
}
