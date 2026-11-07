namespace Torque.Http.Exceptions;

/// <summary>
/// Base exception for all Torque client errors.
/// </summary>
public class TorqueException : Exception
{
    /// <param name="message">Error message.</param>
    public TorqueException(string message) : base(message) { }

    /// <param name="message">Error message.</param>
    /// <param name="innerException">The underlying exception.</param>
    public TorqueException(string message, Exception innerException) : base(message, innerException) { }
}
