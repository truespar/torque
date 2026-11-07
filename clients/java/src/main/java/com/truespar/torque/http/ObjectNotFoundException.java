package com.truespar.torque.http;

/**
 * Thrown when the requested resource does not exist (HTTP 404).
 *
 * <p>Common causes:
 * <ul>
 *   <li>Collection name does not exist</li>
 *   <li>Document ID not found in the collection</li>
 *   <li>Alias, synonym set, preset, or stopword set not found</li>
 * </ul>
 */
public final class ObjectNotFoundException extends TorqueApiException {

    /**
     * Creates a new 404 Not Found exception.
     *
     * @param message server-provided error message
     */
    public ObjectNotFoundException(String message) {
        super(message, 404);
    }
}
