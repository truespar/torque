package com.truespar.torque.http;

/**
 * Thrown when the Torque server returns a non-2xx HTTP response.
 *
 * <p>This is the base class for all HTTP API error responses. Specific subclasses
 * provide finer-grained exception handling:
 *
 * <ul>
 *   <li>{@link BadRequestException} — 400</li>
 *   <li>{@link UnauthorizedException} — 401</li>
 *   <li>{@link ForbiddenException} — 403</li>
 *   <li>{@link ObjectNotFoundException} — 404</li>
 *   <li>{@link ObjectAlreadyExistsException} — 409</li>
 *   <li>{@link ServerErrorException} — 500+</li>
 *   <li>{@link ServiceUnavailableException} — 503</li>
 * </ul>
 *
 * <p>Callers can catch {@code TorqueApiException} for broad error handling or
 * specific subclasses for targeted recovery:
 * <pre>{@code
 * try {
 *     client.getDocument("products", "abc123");
 * } catch (ObjectNotFoundException e) {
 *     // handle missing document
 * } catch (TorqueApiException e) {
 *     // handle all other API errors
 * }
 * }</pre>
 *
 * @see TorqueException
 */
public class TorqueApiException extends TorqueException {
    private final Integer statusCode;

    /**
     * Creates an API exception with a message but no HTTP status code.
     *
     * @param message human-readable error description
     */
    public TorqueApiException(String message) {
        super(message);
        this.statusCode = null;
    }

    /**
     * Creates an API exception with a message and HTTP status code.
     *
     * @param message    human-readable error description (typically from the server response)
     * @param statusCode the HTTP status code (e.g. 404, 500)
     */
    public TorqueApiException(String message, int statusCode) {
        super(message);
        this.statusCode = statusCode;
    }

    /**
     * Creates an API exception wrapping a lower-level cause.
     *
     * @param message human-readable error description
     * @param cause   the underlying exception (e.g. IOException, JsonProcessingException)
     */
    public TorqueApiException(String message, Throwable cause) {
        super(message, cause);
        this.statusCode = null;
    }

    /**
     * Returns the HTTP status code from the server response, or {@code null} if
     * the error did not originate from an HTTP response (e.g. network timeout).
     *
     * @return the HTTP status code, or {@code null}
     */
    public Integer statusCode() { return statusCode; }
}
