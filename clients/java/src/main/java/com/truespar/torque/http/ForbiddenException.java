package com.truespar.torque.http;

/**
 * Thrown when the API key lacks permission for the requested operation (HTTP 403).
 *
 * <p>The key is valid but its scope (collections, actions) does not permit this request.
 * Use an admin or appropriately scoped key instead.
 */
public final class ForbiddenException extends TorqueApiException {

    /**
     * Creates a new 403 Forbidden exception.
     *
     * @param message server-provided error message
     */
    public ForbiddenException(String message) {
        super(message, 403);
    }
}
