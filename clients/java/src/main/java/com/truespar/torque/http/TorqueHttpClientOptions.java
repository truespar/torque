package com.truespar.torque.http;

import java.time.Duration;
import java.util.List;

/**
 * Configuration options for {@link TorqueHttpClient}.
 *
 * <p>Supports both single-node and multi-node configurations. For multi-node setups
 * with automatic failover, provide a list of {@link Node} instances:
 *
 * <pre>{@code
 * // Single node (simple)
 * var options = new TorqueHttpClientOptions("my-api-key");
 * var client = new TorqueHttpClient("http://localhost:8108", options);
 *
 * // Multi-node with failover
 * var options = new TorqueHttpClientOptions(
 *     "my-api-key",
 *     List.of(
 *         new Node("https", "search1.example.com", 8108),
 *         new Node("https", "search2.example.com", 8108),
 *         new Node("https", "search3.example.com", 8108)
 *     ),
 *     3,                           // numRetries
 *     Duration.ofMillis(100),      // retryInterval (base for exponential backoff)
 *     Duration.ofSeconds(60),      // healthcheckInterval
 *     Duration.ofSeconds(30),      // timeout
 *     Duration.ofMinutes(5),       // transferTimeout
 *     false,                       // allowUntrustedCertificate
 *     false                        // sendApiKeyAsQueryParam
 * );
 * var client = new TorqueHttpClient(options);
 * }</pre>
 *
 * @param apiKey                    API key for authentication (X-TYPESENSE-API-KEY header).
 * @param nodes                     list of server nodes for multi-node failover; may be empty for single-node mode.
 * @param numRetries                maximum number of retry attempts per request (default: 3).
 * @param retryInterval             base interval between retries, doubled on each attempt (default: 100ms).
 * @param healthcheckInterval       how often to re-check unhealthy nodes (default: 60s).
 * @param timeout                   timeout for normal API requests (default: 30s).
 * @param transferTimeout           timeout for file transfers and imports (default: 5min).
 * @param allowUntrustedCertificate skip TLS certificate verification (default: false).
 * @param sendApiKeyAsQueryParam    when true, send the API key as {@code ?x-typesense-api-key=KEY}
 *                                  instead of the {@code X-TYPESENSE-API-KEY} header (default: false).
 *
 * @see Node
 * @see TorqueHttpClient
 */
public record TorqueHttpClientOptions(
        String apiKey,
        List<Node> nodes,
        int numRetries,
        Duration retryInterval,
        Duration healthcheckInterval,
        Duration timeout,
        Duration transferTimeout,
        boolean allowUntrustedCertificate,
        boolean sendApiKeyAsQueryParam
) {

    /**
     * Creates options for a single-node client with sensible defaults.
     *
     * @param apiKey API key for authentication
     */
    public TorqueHttpClientOptions(String apiKey) {
        this(apiKey, List.of(), 3, Duration.ofMillis(100), Duration.ofSeconds(60),
                Duration.ofSeconds(30), Duration.ofMinutes(5), false, false);
    }

    /**
     * Creates options with custom timeouts for a single-node client.
     * This constructor preserves backward compatibility with existing code.
     *
     * @param apiKey                    API key for authentication
     * @param timeout                   timeout for normal API requests
     * @param transferTimeout           timeout for file transfers and imports
     * @param allowUntrustedCertificate skip TLS certificate verification
     */
    public TorqueHttpClientOptions(String apiKey, Duration timeout, Duration transferTimeout,
                               boolean allowUntrustedCertificate) {
        this(apiKey, List.of(), 3, Duration.ofMillis(100), Duration.ofSeconds(60),
                timeout, transferTimeout, allowUntrustedCertificate, false);
    }

    /** Compact constructor with validation and defaults for null values. */
    public TorqueHttpClientOptions {
        if (apiKey == null || apiKey.isBlank()) throw new IllegalArgumentException("apiKey is required");
        if (nodes == null) nodes = List.of();
        if (numRetries < 0) numRetries = 3;
        if (retryInterval == null) retryInterval = Duration.ofMillis(100);
        if (healthcheckInterval == null) healthcheckInterval = Duration.ofSeconds(60);
        if (timeout == null) timeout = Duration.ofSeconds(30);
        if (transferTimeout == null) transferTimeout = Duration.ofMinutes(5);
    }
}
