package com.truespar.torque.http;

import java.net.URLEncoder;
import java.nio.charset.StandardCharsets;

/**
 * Options for the document export endpoint.
 *
 * <p>Allows filtering and field projection when exporting documents from a collection.
 *
 * <p>Usage:
 * <pre>{@code
 * var options = new ExportOptions()
 *     .filterBy("status:=active")
 *     .includeFields("id,title,price");
 *
 * String jsonl = client.exportDocuments("products", options);
 * }</pre>
 *
 * @see TorqueHttpClient#exportDocuments(String, ExportOptions)
 */
public final class ExportOptions {

    private String filterBy;
    private String includeFields;
    private String excludeFields;

    /**
     * Sets a filter expression to export only matching documents.
     *
     * @param filterBy Typesense filter syntax (e.g. {@code "status:=active"})
     * @return this builder
     */
    public ExportOptions filterBy(String filterBy) { this.filterBy = filterBy; return this; }

    /**
     * Sets the fields to include in each exported document.
     *
     * @param includeFields comma-separated field names
     * @return this builder
     */
    public ExportOptions includeFields(String includeFields) { this.includeFields = includeFields; return this; }

    /**
     * Sets the fields to exclude from each exported document.
     *
     * @param excludeFields comma-separated field names
     * @return this builder
     */
    public ExportOptions excludeFields(String excludeFields) { this.excludeFields = excludeFields; return this; }

    /** Returns the filter_by expression, or {@code null} if not set. */
    public String filterBy() { return filterBy; }

    /** Returns the include_fields, or {@code null} if not set. */
    public String includeFields() { return includeFields; }

    /** Returns the exclude_fields, or {@code null} if not set. */
    public String excludeFields() { return excludeFields; }

    /**
     * Converts the options to a URL query string fragment.
     *
     * @return URL-encoded query string (without leading {@code ?}), or empty string if no options set
     */
    public String toQueryString() {
        var sb = new StringBuilder();
        if (filterBy != null) append(sb, "filter_by", filterBy);
        if (includeFields != null) append(sb, "include_fields", includeFields);
        if (excludeFields != null) append(sb, "exclude_fields", excludeFields);
        return sb.toString();
    }

    private static void append(StringBuilder sb, String key, String value) {
        if (!sb.isEmpty()) sb.append('&');
        sb.append(URLEncoder.encode(key, StandardCharsets.UTF_8))
          .append('=')
          .append(URLEncoder.encode(value, StandardCharsets.UTF_8).replace("+", "%20"));
    }
}
