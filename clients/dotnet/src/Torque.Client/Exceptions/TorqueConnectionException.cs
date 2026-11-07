namespace Torque.Http.Exceptions;

/// <summary>
/// Exception for TCP/HTTP connection failures.
/// </summary>
public sealed class TorqueConnectionException : TorqueException
{
    /// <param name="message">Error message describing the connection failure.</param>
    public TorqueConnectionException(string message) : base(message) { }

    /// <param name="message">Error message describing the connection failure.</param>
    /// <param name="innerException">The underlying exception that caused the connection failure.</param>
    public TorqueConnectionException(string message, Exception innerException) : base(message, innerException) { }
}
