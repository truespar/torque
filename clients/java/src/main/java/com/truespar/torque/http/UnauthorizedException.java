package com.truespar.torque.http;

/**
 * Thrown when the API key is missing or invalid (HTTP 401).
 *
 * <p>Verify that the correct API key is set in {@link TorqueHttpClientOptions}
 * and that the key has not been revoked on the server.
 */
public final class UnauthorizedException extends TorqueApiException {

    /**
     * Creates a new 401 Unauthorized exception.
     *
     * @param message server-provided error message
     */
    public UnauthorizedException(String message) {
        super(message, 401);
    }
}
