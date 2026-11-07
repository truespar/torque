namespace Torque.Http.Tcp;

/// <summary>
/// Protocol constants matching src/ingest/protocol.rs.
/// </summary>
internal static class ProtocolConstants
{
    // Client → Server message types
    public const byte MsgStartIngest = 0x01;
    public const byte MsgDocBatch = 0x02;
    public const byte MsgCommit = 0x03;
    public const byte MsgAbort = 0x04;
    public const byte MsgUpsert = 0x05;

    // Server → Client message types
    public const byte MsgAck = 0x10;
    public const byte MsgCommitResult = 0x11;
    public const byte MsgError = 0x12;
    public const byte MsgBackpressure = 0x13;

    // Error codes
    public const ushort ErrCollectionNotFound = 1;
    public const ushort ErrSchemaMismatch = 2;
    public const ushort ErrSessionAlreadyActive = 3;
    public const ushort ErrNoActiveSession = 4;
    public const ushort ErrBatchTooLarge = 5;
    public const ushort ErrInternal = 6;
    public const ushort ErrBackpressure = 8;

    // Upsert action codes
    public const byte ActionUpsert = 0;
    public const byte ActionDelete = 1;
}
