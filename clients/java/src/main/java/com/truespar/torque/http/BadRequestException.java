package com.truespar.torque.http;

/**
 * Thrown when the request is malformed or contains invalid parameters (HTTP 400).
 *
 * <p>Common causes:
 * <ul>
 *   <li>Invalid search parameters (unknown field in query_by, bad filter syntax)</li>
 *   <li>Malformed JSON body</li>
 *   <li>Missing required fields in a schema definition</li>
 * </ul>
 */
public final class BadRequestException extends TorqueApiException {

    /**
     * Creates a new 400 Bad Request exception.
     *
     * @param message server-provided error message
     */
    public BadRequestException(String message) {
        super(message, 400);
    }
}
