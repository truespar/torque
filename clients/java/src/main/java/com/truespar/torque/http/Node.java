package com.truespar.torque.http;

/**
 * Configuration for a single Torque server node used in multi-node failover.
 *
 * <p>Nodes are specified when constructing {@link TorqueHttpClientOptions} to enable
 * automatic failover and load distribution across multiple Torque servers.
 *
 * <p>Usage:
 * <pre>{@code
 * var node = new Node("https", "search1.example.com", 8108);
 * }</pre>
 *
 * @param protocol the protocol to use ({@code "http"} or {@code "https"})
 * @param host     the hostname or IP address
 * @param port     the port number
 *
 * @see TorqueHttpClientOptions
 */
public record Node(String protocol, String host, int port) {

    /**
     * Creates a new node configuration.
     *
     * @param protocol the protocol ({@code "http"} or {@code "https"})
     * @param host     the hostname or IP address; must not be blank
     * @param port     the port number; must be between 1 and 65535
     */
    public Node {
        if (protocol == null || protocol.isBlank()) throw new IllegalArgumentException("protocol is required");
        if (host == null || host.isBlank()) throw new IllegalArgumentException("host is required");
        if (port < 1 || port > 65535) throw new IllegalArgumentException("port must be between 1 and 65535");
    }

    /**
     * Returns the base URL for this node (e.g. {@code "https://search1.example.com:8108/"}).
     *
     * @return the base URL string with trailing slash
     */
    public String baseUrl() {
        return protocol + "://" + host + ":" + port + "/";
    }
}
