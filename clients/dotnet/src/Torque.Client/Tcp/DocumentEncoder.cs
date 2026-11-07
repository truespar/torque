using System.Buffers.Binary;
using System.Text;
using Torque.Http.Models;

namespace Torque.Http.Tcp;

/// <summary>
/// Encodes TorqueDocument into binary format matching serialize_document in protocol.rs.
/// All multi-byte values are big-endian.
/// Uses (byte[] buf, ref int pos) pattern for zero-allocation writes.
/// Caller must ensure buf has sufficient capacity via ComputeSize().
///
/// String byte lengths are cached on FieldValue at construction time,
/// so ComputeSize() is O(field_count) with no UTF-8 scanning,
/// and Encode() never calls GetByteCount — only GetBytes.
/// </summary>
internal static class DocumentEncoder
{
    /// <summary>
    /// Encode a single document directly into buf at pos.
    /// Caller must ensure buf has at least ComputeSize() bytes available from pos.
    /// </summary>
    public static void Encode(TorqueDocument doc, CollectionSchema schema, byte[] buf, ref int pos)
    {
        // External ID (u16 length prefix) — use cached byte length
        WriteStrU16(buf, ref pos, doc.Id, doc.IdUtf8ByteLength);

        // Language byte
        buf[pos++] = (byte)doc.Language;

        // Field count (u16)
        BinaryPrimitives.WriteUInt16BigEndian(buf.AsSpan(pos), (ushort)schema.Fields.Count);
        pos += 2;

        // Fields in schema order
        for (int i = 0; i < schema.Fields.Count; i++)
        {
            var value = i < doc.Fields.Length ? doc.Fields[i] : null;

            if (value is null || value.IsNull)
            {
                buf[pos++] = 0; // null
            }
            else
            {
                buf[pos++] = 1; // present
                WriteFieldValue(buf, ref pos, value);
            }
        }
    }

    /// <summary>
    /// Compute exact encoded size of a document (for buffer capacity pre-check).
    /// O(field_count) — no UTF-8 scanning, uses cached byte lengths.
    /// </summary>
    public static int ComputeSize(TorqueDocument doc, CollectionSchema schema)
    {
        // u16 id_len + id_bytes + u8 language + u16 field_count
        int size = 2 + doc.IdUtf8ByteLength + 1 + 2;

        for (int i = 0; i < schema.Fields.Count; i++)
        {
            size += 1; // presence byte
            var val = i < doc.Fields.Length ? doc.Fields[i] : null;
            if (val is not null && !val.IsNull)
                size += ComputeFieldSize(val);
        }

        return size;
    }

    private static int ComputeFieldSize(FieldValue value) => value switch
    {
        FieldValue.StringValue s => 4 + s.Utf8ByteLength,
        FieldValue.Int32Value => 4,
        FieldValue.Int64Value => 8,
        FieldValue.Float64Value => 8,
        FieldValue.BoolValue => 1,
        FieldValue.GeoPointValue => 16,
        FieldValue.StringArrayValue sa => 2 + ComputeStringArraySize(sa),
        FieldValue.Int32ArrayValue ia => 2 + ComputeInt32ArraySize(ia),
        FieldValue.Int64ArrayValue la => 2 + ComputeInt64ArraySize(la),
        FieldValue.FloatArrayValue fa => 2 + fa.Values.Length * 4,
        _ => 0,
    };

    private static int ComputeStringArraySize(FieldValue.StringArrayValue sa)
    {
        int size = 0;
        for (int i = 0; i < sa.Values.Length; i++)
        {
            size += 1; // null flag
            if (sa.Values[i] is not null)
                size += 4 + sa.Utf8ByteLengths[i]; // u32 len + string bytes
        }
        return size;
    }

    private static int ComputeInt32ArraySize(FieldValue.Int32ArrayValue ia)
    {
        int size = 0;
        for (int i = 0; i < ia.Values.Length; i++)
        {
            size += 1; // null flag
            if (ia.Values[i].HasValue)
                size += 4;
        }
        return size;
    }

    private static int ComputeInt64ArraySize(FieldValue.Int64ArrayValue la)
    {
        int size = 0;
        for (int i = 0; i < la.Values.Length; i++)
        {
            size += 1; // null flag
            if (la.Values[i].HasValue)
                size += 8;
        }
        return size;
    }

    private static void WriteFieldValue(byte[] buf, ref int pos, FieldValue value)
    {
        switch (value)
        {
            case FieldValue.StringValue s:
                WriteStr(buf, ref pos, s.Value, s.Utf8ByteLength);
                break;

            case FieldValue.Int32Value i:
                BinaryPrimitives.WriteInt32BigEndian(buf.AsSpan(pos), i.Value);
                pos += 4;
                break;

            case FieldValue.Int64Value l:
                BinaryPrimitives.WriteInt64BigEndian(buf.AsSpan(pos), l.Value);
                pos += 8;
                break;

            case FieldValue.Float64Value f:
                BinaryPrimitives.WriteDoubleBigEndian(buf.AsSpan(pos), f.Value);
                pos += 8;
                break;

            case FieldValue.BoolValue b:
                buf[pos++] = b.Value ? (byte)1 : (byte)0;
                break;

            case FieldValue.GeoPointValue g:
                BinaryPrimitives.WriteDoubleBigEndian(buf.AsSpan(pos), g.Lat);
                pos += 8;
                BinaryPrimitives.WriteDoubleBigEndian(buf.AsSpan(pos), g.Lng);
                pos += 8;
                break;

            case FieldValue.StringArrayValue sa:
                BinaryPrimitives.WriteUInt16BigEndian(buf.AsSpan(pos), (ushort)sa.Values.Length);
                pos += 2;
                for (int j = 0; j < sa.Values.Length; j++)
                {
                    if (sa.Values[j] is null)
                    {
                        buf[pos++] = 0; // null
                    }
                    else
                    {
                        buf[pos++] = 1; // present
                        WriteStr(buf, ref pos, sa.Values[j]!, sa.Utf8ByteLengths[j]);
                    }
                }
                break;

            case FieldValue.Int32ArrayValue ia:
                BinaryPrimitives.WriteUInt16BigEndian(buf.AsSpan(pos), (ushort)ia.Values.Length);
                pos += 2;
                for (int j = 0; j < ia.Values.Length; j++)
                {
                    if (!ia.Values[j].HasValue)
                    {
                        buf[pos++] = 0; // null
                    }
                    else
                    {
                        buf[pos++] = 1; // present
                        BinaryPrimitives.WriteInt32BigEndian(buf.AsSpan(pos), ia.Values[j]!.Value);
                        pos += 4;
                    }
                }
                break;

            case FieldValue.Int64ArrayValue la:
                BinaryPrimitives.WriteUInt16BigEndian(buf.AsSpan(pos), (ushort)la.Values.Length);
                pos += 2;
                for (int j = 0; j < la.Values.Length; j++)
                {
                    if (!la.Values[j].HasValue)
                    {
                        buf[pos++] = 0; // null
                    }
                    else
                    {
                        buf[pos++] = 1; // present
                        BinaryPrimitives.WriteInt64BigEndian(buf.AsSpan(pos), la.Values[j]!.Value);
                        pos += 8;
                    }
                }
                break;

            case FieldValue.FloatArrayValue fa:
                BinaryPrimitives.WriteUInt16BigEndian(buf.AsSpan(pos), (ushort)fa.Values.Length);
                pos += 2;
                for (int j = 0; j < fa.Values.Length; j++)
                {
                    BinaryPrimitives.WriteSingleBigEndian(buf.AsSpan(pos), fa.Values[j]);
                    pos += 4;
                }
                break;
        }
    }

    /// <summary>
    /// Write string with u32 big-endian length prefix (matches Rust write_str).
    /// Uses pre-cached byteLen — no GetByteCount call.
    /// </summary>
    private static void WriteStr(byte[] buf, ref int pos, string s, int byteLen)
    {
        BinaryPrimitives.WriteUInt32BigEndian(buf.AsSpan(pos), (uint)byteLen);
        Encoding.UTF8.GetBytes(s, buf.AsSpan(pos + 4));
        pos += 4 + byteLen;
    }

    /// <summary>
    /// Write string with u16 big-endian length prefix (matches Rust write_str_u16).
    /// Uses pre-cached byteLen — no GetByteCount call.
    /// </summary>
    private static void WriteStrU16(byte[] buf, ref int pos, string s, int byteLen)
    {
        BinaryPrimitives.WriteUInt16BigEndian(buf.AsSpan(pos), (ushort)byteLen);
        Encoding.UTF8.GetBytes(s, buf.AsSpan(pos + 2));
        pos += 2 + byteLen;
    }
}
