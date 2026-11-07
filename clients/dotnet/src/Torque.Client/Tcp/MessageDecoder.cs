using System.Buffers.Binary;
using System.Text;
using Torque.Http.Exceptions;
using Torque.Http.Models;

namespace Torque.Http.Tcp;

/// <summary>
/// Decodes server→client messages matching deserialize_server_message in protocol.rs.
/// </summary>
internal static class MessageDecoder
{
    /// <summary>
    /// Decode a server message from raw frame bytes.
    /// Returns either BatchAck, CommitResult, or throws TorqueProtocolException for Error messages.
    /// </summary>
    public static object Decode(ReadOnlySpan<byte> data)
    {
        if (data.IsEmpty)
            throw new TorqueProtocolException(0, "Empty server message");

        byte msgType = data[0];
        var payload = data[1..];

        return msgType switch
        {
            ProtocolConstants.MsgAck => DecodeAck(payload),
            ProtocolConstants.MsgCommitResult => DecodeCommitResult(payload),
            ProtocolConstants.MsgBackpressure => DecodeBackpressure(payload),
            ProtocolConstants.MsgError => throw DecodeError(payload),
            _ => throw new TorqueProtocolException(0, $"Unknown server message type: 0x{msgType:X2}"),
        };
    }

    private static BatchAck DecodeAck(ReadOnlySpan<byte> data)
    {
        // [u64 batch_seq][u64 docs_received]
        if (data.Length < 16)
            throw new TorqueProtocolException(0, "Ack message too short");

        return new BatchAck
        {
            BatchSeq = BinaryPrimitives.ReadUInt64BigEndian(data),
            DocsReceived = BinaryPrimitives.ReadUInt64BigEndian(data[8..]),
        };
    }

    private static CommitResult DecodeCommitResult(ReadOnlySpan<byte> data)
    {
        // [u8 success][u64 docs_indexed][u64 build_ms]
        if (data.Length < 17)
            throw new TorqueProtocolException(0, "CommitResult message too short");

        return new CommitResult
        {
            Success = data[0] != 0,
            DocsIndexed = BinaryPrimitives.ReadUInt64BigEndian(data[1..]),
            BuildMs = BinaryPrimitives.ReadUInt64BigEndian(data[9..]),
        };
    }

    private static BackpressureResult DecodeBackpressure(ReadOnlySpan<byte> data)
    {
        // [u32 retry_after_ms]
        if (data.Length < 4)
            throw new TorqueProtocolException(0, "Backpressure message too short");

        return new BackpressureResult
        {
            RetryAfterMs = BinaryPrimitives.ReadUInt32BigEndian(data),
        };
    }

    private static TorqueProtocolException DecodeError(ReadOnlySpan<byte> data)
    {
        // [u16 code][u32 msg_len][UTF-8 message]
        if (data.Length < 6)
            return new TorqueProtocolException(0, "Error message too short");

        ushort code = BinaryPrimitives.ReadUInt16BigEndian(data);
        uint msgLen = BinaryPrimitives.ReadUInt32BigEndian(data[2..]);

        string message = msgLen > 0 && msgLen <= (uint)(data.Length - 6)
            ? Encoding.UTF8.GetString(data.Slice(6, (int)msgLen))
            : "Unknown error";

        return new TorqueProtocolException(code, message);
    }
}
