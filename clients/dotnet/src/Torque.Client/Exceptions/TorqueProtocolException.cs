namespace Torque.Http.Exceptions;

/// <summary>
/// Exception for TCP binary protocol errors from the server.
/// </summary>
public sealed class TorqueProtocolException : TorqueException
{
    /// <summary>Protocol-level error code from the TCP binary ingest response.</summary>
    public ushort ErrorCode { get; }

    /// <summary>
    /// Initializes a new instance with the protocol error code and message.
    /// </summary>
    /// <param name="errorCode">The binary protocol error code.</param>
    /// <param name="message">The error message from the server.</param>
    public TorqueProtocolException(ushort errorCode, string message)
        : base(message)
    {
        ErrorCode = errorCode;
    }
}
