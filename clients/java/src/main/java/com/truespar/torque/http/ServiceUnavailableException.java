package com.truespar.torque.http;

/**
 * Thrown when the server is temporarily unavailable (HTTP 503).
 *
 * <p>This usually means the server is starting up, shutting down, or overloaded.
 * Multi-node failover will route subsequent requests to other healthy nodes.
 * The failed node is marked unhealthy and periodically rechecked.
 */
public final class ServiceUnavailableException extends TorqueApiException {

    /**
     * Creates a new 503 Service Unavailable exception.
     *
     * @param message server-provided error message
     */
    public ServiceUnavailableException(String message) {
        super(message, 503);
    }
}
