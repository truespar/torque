using System.Buffers.Binary;
using System.Text;

namespace Torque.Http.Tcp;

/// <summary>
/// Encodes client→server messages matching serialize_client_message in protocol.rs.
/// All multi-byte values are big-endian.
/// Uses (byte[] buf, ref int pos) pattern for zero-allocation writes.
/// </summary>
internal static class MessageEncoder
{
    /// <summary>
    /// Encode StartIngest: [0x01][u32 len][collection UTF-8][u8 mode]
    /// </summary>
    public static void EncodeStartIngest(byte[] buf, ref int pos, string collection, byte mode = 0)
    {
        buf[pos++] = ProtocolConstants.MsgStartIngest;
        int byteCount = Encoding.UTF8.GetByteCount(collection);
        BinaryPrimitives.WriteUInt32BigEndian(buf.AsSpan(pos), (uint)byteCount);
        pos += 4;
        Encoding.UTF8.GetBytes(collection, buf.AsSpan(pos));
        pos += byteCount;
        buf[pos++] = mode;
    }

    /// <summary>
    /// Compute exact encoded size for StartIngest message.
    /// </summary>
    public static int ComputeStartIngestSize(string collection) =>
        1 + 4 + Encoding.UTF8.GetByteCount(collection) + 1;

    /// <summary>
    /// Encode DocBatch header only: [0x02][u32 doc_count]
    /// Document payloads are written separately by DocumentEncoder.
    /// </summary>
    public static void EncodeDocBatchHeader(byte[] buf, ref int pos, uint docCount)
    {
        buf[pos++] = ProtocolConstants.MsgDocBatch;
        BinaryPrimitives.WriteUInt32BigEndian(buf.AsSpan(pos), docCount);
        pos += 4;
    }

    /// <summary>
    /// Encode Commit: [0x03]
    /// </summary>
    public static void EncodeCommit(byte[] buf, ref int pos) =>
        buf[pos++] = ProtocolConstants.MsgCommit;

    /// <summary>
    /// Encode Abort: [0x04]
    /// </summary>
    public static void EncodeAbort(byte[] buf, ref int pos) =>
        buf[pos++] = ProtocolConstants.MsgAbort;

    /// <summary>
    /// Encode Upsert header: [0x05][u32 coll_len][coll_bytes][u8 action][u32 doc_count]
    /// Document or delete-ID payloads are written separately after this header.
    /// </summary>
    public static void EncodeUpsertHeader(byte[] buf, ref int pos, string collection, byte action, uint docCount)
    {
        buf[pos++] = ProtocolConstants.MsgUpsert;
        int byteCount = Encoding.UTF8.GetByteCount(collection);
        BinaryPrimitives.WriteUInt32BigEndian(buf.AsSpan(pos), (uint)byteCount);
        pos += 4;
        Encoding.UTF8.GetBytes(collection, buf.AsSpan(pos));
        pos += byteCount;
        buf[pos++] = action;
        BinaryPrimitives.WriteUInt32BigEndian(buf.AsSpan(pos), docCount);
        pos += 4;
    }

    /// <summary>
    /// Compute exact encoded size for Upsert header (without document/ID payload).
    /// </summary>
    public static int ComputeUpsertHeaderSize(string collection) =>
        1 + 4 + Encoding.UTF8.GetByteCount(collection) + 1 + 4;

    /// <summary>
    /// Encode delete IDs payload: [u16 id_len][id_bytes] for each ID.
    /// </summary>
    public static void EncodeDeleteIds(byte[] buf, ref int pos, IReadOnlyList<string> ids)
    {
        for (int i = 0; i < ids.Count; i++)
        {
            int idLen = Encoding.UTF8.GetByteCount(ids[i]);
            BinaryPrimitives.WriteUInt16BigEndian(buf.AsSpan(pos), (ushort)idLen);
            pos += 2;
            Encoding.UTF8.GetBytes(ids[i], buf.AsSpan(pos));
            pos += idLen;
        }
    }

    /// <summary>
    /// Compute exact encoded size for delete IDs payload.
    /// </summary>
    public static int ComputeDeleteIdsSize(IReadOnlyList<string> ids)
    {
        int total = 0;
        for (int i = 0; i < ids.Count; i++)
            total += 2 + Encoding.UTF8.GetByteCount(ids[i]);
        return total;
    }
}
