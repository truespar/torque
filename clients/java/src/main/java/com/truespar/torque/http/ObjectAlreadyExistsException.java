package com.truespar.torque.http;

/**
 * Thrown when attempting to create a resource that already exists (HTTP 409).
 *
 * <p>Common causes:
 * <ul>
 *   <li>Creating a collection with a name that is already in use</li>
 *   <li>Creating a document with an ID that already exists (without upsert)</li>
 * </ul>
 */
public final class ObjectAlreadyExistsException extends TorqueApiException {

    /**
     * Creates a new 409 Conflict exception.
     *
     * @param message server-provided error message
     */
    public ObjectAlreadyExistsException(String message) {
        super(message, 409);
    }
}
