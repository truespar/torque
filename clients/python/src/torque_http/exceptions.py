"""Torque client exceptions — typed error hierarchy.

Maps HTTP status codes to specific exception types for precise
error handling. Zero external dependencies.
"""

from __future__ import annotations


class TorqueError(Exception):
    """Base exception for all Torque client errors."""


class TorqueApiError(TorqueError):
    """Raised when the Torque server returns a non-2xx response.

    Attributes:
        status_code: HTTP status code from the server.
        message: Human-readable error message.
    """

    def __init__(self, message: str, *, status_code: int) -> None:
        super().__init__(message)
        self.status_code = status_code
        self.message = message


class ObjectNotFound(TorqueApiError):
    """Raised when the requested resource does not exist (HTTP 404)."""


class ObjectAlreadyExists(TorqueApiError):
    """Raised when the resource already exists (HTTP 409)."""


class RequestUnauthorized(TorqueApiError):
    """Raised when authentication fails (HTTP 401)."""


class RequestForbidden(TorqueApiError):
    """Raised when the API key lacks required permissions (HTTP 403)."""


class RequestMalformed(TorqueApiError):
    """Raised when the request is invalid (HTTP 400)."""


class ServerError(TorqueApiError):
    """Raised when the server encounters an internal error (HTTP 500+)."""


class ServiceUnavailable(TorqueApiError):
    """Raised when the server is temporarily unavailable (HTTP 503)."""


class RequestTimeout(TorqueApiError):
    """Raised when the request times out."""

    def __init__(self, message: str = "Request timed out") -> None:
        super().__init__(message, status_code=408)


# Keep backward compatibility: TorqueApiException is an alias
TorqueApiException = TorqueApiError


# ── Status code -> exception mapping ──────────────────────────────────

_STATUS_EXCEPTION_MAP: dict[int, type[TorqueApiError]] = {
    400: RequestMalformed,
    401: RequestUnauthorized,
    403: RequestForbidden,
    404: ObjectNotFound,
    408: RequestTimeout,
    409: ObjectAlreadyExists,
    503: ServiceUnavailable,
}


def raise_for_status(status_code: int, message: str) -> None:
    """Raise the appropriate typed exception for the given HTTP status code.

    Args:
        status_code: HTTP status code from the server response.
        message: Error message from the server or a default.

    Raises:
        TorqueApiError: A subclass matching the status code, or the base
            class for unrecognized codes.
    """
    exc_cls = _STATUS_EXCEPTION_MAP.get(status_code)
    if exc_cls is not None:
        raise exc_cls(message, status_code=status_code)
    if status_code >= 500:
        raise ServerError(message, status_code=status_code)
    raise TorqueApiError(message, status_code=status_code)
