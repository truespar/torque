package com.truespar.torque.http;

/**
 * Thrown when the server encounters an internal error (HTTP 500+).
 *
 * <p>This typically indicates a bug in the server or an unexpected condition.
 * If using multi-node failover, the client will automatically retry on the next
 * healthy node before surfacing this exception.
 */
public final class ServerErrorException extends TorqueApiException {

    /**
     * Creates a new server error exception.
     *
     * @param message    server-provided error message
     * @param statusCode the HTTP status code (500, 502, etc.)
     */
    public ServerErrorException(String message, int statusCode) {
        super(message, statusCode);
    }
}
