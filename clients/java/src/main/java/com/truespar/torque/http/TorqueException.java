package com.truespar.torque.http;

/**
 * Base exception for all Torque client errors.
 *
 * <p>This is the root of the Torque exception hierarchy. It covers both
 * HTTP API errors (via {@link TorqueApiException} subclasses) and local
 * client-side failures such as network timeouts or serialization errors.
 *
 * @see TorqueApiException
 */
public class TorqueException extends RuntimeException {

    /**
     * Creates a new Torque exception with the given message.
     *
     * @param message human-readable error description
     */
    public TorqueException(String message) {
        super(message);
    }

    /**
     * Creates a new Torque exception with the given message and cause.
     *
     * @param message human-readable error description
     * @param cause   the underlying exception
     */
    public TorqueException(String message, Throwable cause) {
        super(message, cause);
    }
}
