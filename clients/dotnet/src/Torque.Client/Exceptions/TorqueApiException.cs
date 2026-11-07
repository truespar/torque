using System.Net;

namespace Torque.Http.Exceptions;

/// <summary>
/// Exception for HTTP API errors. Contains the status code and server error message.
/// </summary>
public class TorqueApiException : TorqueException
{
    /// <summary>HTTP status code returned by the server.</summary>
    public HttpStatusCode StatusCode { get; }

    /// <summary>
    /// Initializes a new instance with the HTTP status code and error message from the server.
    /// </summary>
    /// <param name="statusCode">The HTTP status code.</param>
    /// <param name="message">The error message from the server response body.</param>
    public TorqueApiException(HttpStatusCode statusCode, string message)
        : base(message)
    {
        StatusCode = statusCode;
    }

    internal static TorqueApiException FromStatusCode(HttpStatusCode statusCode, string message) => statusCode switch
    {
        HttpStatusCode.BadRequest => new BadRequestException(message),
        HttpStatusCode.Unauthorized => new UnauthorizedException(message),
        HttpStatusCode.Forbidden => new ForbiddenException(message),
        HttpStatusCode.NotFound => new ObjectNotFoundException(message),
        HttpStatusCode.Conflict => new ObjectAlreadyExistsException(message),
        HttpStatusCode.ServiceUnavailable => new ServiceUnavailableException(message),
        >= HttpStatusCode.InternalServerError => new ServerErrorException(statusCode, message),
        _ => new TorqueApiException(statusCode, message),
    };
}

/// <summary>
/// The request was malformed or contained invalid parameters (HTTP 400).
/// </summary>
public sealed class BadRequestException(string message)
    : TorqueApiException(HttpStatusCode.BadRequest, message);

/// <summary>
/// Authentication failed — invalid or missing API key (HTTP 401).
/// </summary>
public sealed class UnauthorizedException(string message)
    : TorqueApiException(HttpStatusCode.Unauthorized, message);

/// <summary>
/// The API key does not have permission for this operation (HTTP 403).
/// </summary>
public sealed class ForbiddenException(string message)
    : TorqueApiException(HttpStatusCode.Forbidden, message);

/// <summary>
/// The requested resource (collection, document, key, etc.) was not found (HTTP 404).
/// </summary>
public sealed class ObjectNotFoundException(string message)
    : TorqueApiException(HttpStatusCode.NotFound, message);

/// <summary>
/// The resource already exists (e.g. creating a collection that already exists) (HTTP 409).
/// </summary>
public sealed class ObjectAlreadyExistsException(string message)
    : TorqueApiException(HttpStatusCode.Conflict, message);

/// <summary>
/// Internal server error (HTTP 500+).
/// </summary>
public sealed class ServerErrorException(HttpStatusCode statusCode, string message)
    : TorqueApiException(statusCode, message);

/// <summary>
/// The server is temporarily unavailable (HTTP 503).
/// </summary>
public sealed class ServiceUnavailableException(string message)
    : TorqueApiException(HttpStatusCode.ServiceUnavailable, message);
