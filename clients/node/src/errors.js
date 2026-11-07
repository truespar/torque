/**
 * Base error for all Torque SDK errors.
 * All errors thrown by the SDK extend this class.
 */
export class TorqueError extends Error {
  /**
   * @param {string} message - Human-readable error description.
   */
  constructor(message) {
    super(message);
    this.name = 'TorqueError';
  }
}

/**
 * Thrown when the Torque server returns a non-2xx HTTP response.
 * Contains the HTTP status code and server-provided error message.
 */
export class TorqueApiError extends TorqueError {
  /**
   * @param {string} message - Error message from server or default.
   * @param {number} statusCode - HTTP status code.
   */
  constructor(message, statusCode) {
    super(message);
    this.name = 'TorqueApiError';
    /** @type {number} HTTP status code of the failed request. */
    this.statusCode = statusCode;
  }
}

/**
 * HTTP 400 — The request was malformed or contained invalid parameters.
 */
export class RequestMalformed extends TorqueApiError {
  /**
   * @param {string} message
   */
  constructor(message) {
    super(message, 400);
    this.name = 'RequestMalformed';
  }
}

/**
 * HTTP 401 — The API key is missing or invalid.
 */
export class RequestUnauthorized extends TorqueApiError {
  /**
   * @param {string} message
   */
  constructor(message) {
    super(message, 401);
    this.name = 'RequestUnauthorized';
  }
}

/**
 * HTTP 403 — The API key does not have permission for this operation.
 */
export class RequestForbidden extends TorqueApiError {
  /**
   * @param {string} message
   */
  constructor(message) {
    super(message, 403);
    this.name = 'RequestForbidden';
  }
}

/**
 * HTTP 404 — The requested resource (collection, document, alias, etc.) was not found.
 */
export class ObjectNotFound extends TorqueApiError {
  /**
   * @param {string} message
   */
  constructor(message) {
    super(message, 404);
    this.name = 'ObjectNotFound';
  }
}

/**
 * HTTP 409 — The resource already exists (e.g. duplicate collection name).
 */
export class ObjectAlreadyExists extends TorqueApiError {
  /**
   * @param {string} message
   */
  constructor(message) {
    super(message, 409);
    this.name = 'ObjectAlreadyExists';
  }
}

/**
 * HTTP 500+ — An internal server error occurred.
 */
export class ServerError extends TorqueApiError {
  /**
   * @param {string} message
   * @param {number} [statusCode=500]
   */
  constructor(message, statusCode = 500) {
    super(message, statusCode);
    this.name = 'ServerError';
  }
}

/**
 * HTTP 503 — The server is temporarily unavailable (e.g. during index rebuild).
 */
export class ServiceUnavailable extends TorqueApiError {
  /**
   * @param {string} message
   */
  constructor(message) {
    super(message, 503);
    this.name = 'ServiceUnavailable';
  }
}

/**
 * Thrown when a request exceeds the configured timeout or the server does not respond.
 */
export class RequestTimeout extends TorqueApiError {
  /**
   * @param {string} message
   */
  constructor(message) {
    super(message, 408);
    this.name = 'RequestTimeout';
  }
}

/**
 * Map an HTTP status code to the appropriate TorqueApiError subclass.
 *
 * @param {number} statusCode - HTTP status code.
 * @param {string} message - Error message.
 * @returns {TorqueApiError} The most specific error instance for the status code.
 */
export function errorForStatusCode(statusCode, message) {
  switch (statusCode) {
    case 400: return new RequestMalformed(message);
    case 401: return new RequestUnauthorized(message);
    case 403: return new RequestForbidden(message);
    case 404: return new ObjectNotFound(message);
    case 409: return new ObjectAlreadyExists(message);
    case 503: return new ServiceUnavailable(message);
    default:
      if (statusCode >= 500) return new ServerError(message, statusCode);
      return new TorqueApiError(message, statusCode);
  }
}
