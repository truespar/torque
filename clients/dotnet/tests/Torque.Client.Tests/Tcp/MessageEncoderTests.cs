using System.Buffers.Binary;
using System.Text;
using Torque.Http.Tcp;

namespace Torque.Http.Tests.Tcp;

public class MessageEncoderTests
{
    [Fact]
    public void EncodeStartIngest_MatchesRustFormat()
    {
        int size = MessageEncoder.ComputeStartIngestSize("products");
        var buf = new byte[size];
        int pos = 0;
        MessageEncoder.EncodeStartIngest(buf, ref pos, "products", 0);

        int offset = 0;
        Assert.Equal(ProtocolConstants.MsgStartIngest, buf[offset++]);

        uint nameLen = BinaryPrimitives.ReadUInt32BigEndian(buf.AsSpan(offset));
        offset += 4;
        Assert.Equal(8u, nameLen); // "products" = 8 bytes

        string name = Encoding.UTF8.GetString(buf, offset, (int)nameLen);
        offset += (int)nameLen;
        Assert.Equal("products", name);

        Assert.Equal(0, buf[offset]); // mode
        Assert.Equal(size, pos); // all bytes written
    }

    [Fact]
    public void EncodeDocBatchHeader_MatchesRustFormat()
    {
        var buf = new byte[16];
        int pos = 0;
        MessageEncoder.EncodeDocBatchHeader(buf, ref pos, 3);

        Assert.Equal(ProtocolConstants.MsgDocBatch, buf[0]);
        Assert.Equal(3u, BinaryPrimitives.ReadUInt32BigEndian(buf.AsSpan(1)));
        Assert.Equal(5, pos); // 1 byte msg type + 4 bytes doc count
    }

    [Fact]
    public void EncodeCommit_SingleByte()
    {
        var buf = new byte[4];
        int pos = 0;
        MessageEncoder.EncodeCommit(buf, ref pos);
        Assert.Equal(1, pos);
        Assert.Equal(ProtocolConstants.MsgCommit, buf[0]);
    }

    [Fact]
    public void EncodeAbort_SingleByte()
    {
        var buf = new byte[4];
        int pos = 0;
        MessageEncoder.EncodeAbort(buf, ref pos);
        Assert.Equal(1, pos);
        Assert.Equal(ProtocolConstants.MsgAbort, buf[0]);
    }

    [Fact]
    public void EncodeStartIngest_Utf8Collection()
    {
        // Swedish characters
        int size = MessageEncoder.ComputeStartIngestSize("produkter_ö");
        var buf = new byte[size];
        int pos = 0;
        MessageEncoder.EncodeStartIngest(buf, ref pos, "produkter_ö", 0);

        Assert.Equal(ProtocolConstants.MsgStartIngest, buf[0]);
        uint nameLen = BinaryPrimitives.ReadUInt32BigEndian(buf.AsSpan(1));
        var name = Encoding.UTF8.GetString(buf, 5, (int)nameLen);
        Assert.Equal("produkter_ö", name);
        Assert.Equal(size, pos);
    }

    [Fact]
    public void EncodeUpsertHeader_Upsert_MatchesRustFormat()
    {
        int size = MessageEncoder.ComputeUpsertHeaderSize("products");
        var buf = new byte[size];
        int pos = 0;
        MessageEncoder.EncodeUpsertHeader(buf, ref pos, "products", ProtocolConstants.ActionUpsert, 10);

        int offset = 0;
        Assert.Equal(ProtocolConstants.MsgUpsert, buf[offset++]);

        uint nameLen = BinaryPrimitives.ReadUInt32BigEndian(buf.AsSpan(offset));
        offset += 4;
        Assert.Equal(8u, nameLen);

        string name = Encoding.UTF8.GetString(buf, offset, (int)nameLen);
        offset += (int)nameLen;
        Assert.Equal("products", name);

        Assert.Equal(ProtocolConstants.ActionUpsert, buf[offset++]);

        uint docCount = BinaryPrimitives.ReadUInt32BigEndian(buf.AsSpan(offset));
        Assert.Equal(10u, docCount);

        Assert.Equal(size, pos);
    }

    [Fact]
    public void EncodeUpsertHeader_Delete_MatchesRustFormat()
    {
        int size = MessageEncoder.ComputeUpsertHeaderSize("products");
        var buf = new byte[size];
        int pos = 0;
        MessageEncoder.EncodeUpsertHeader(buf, ref pos, "products", ProtocolConstants.ActionDelete, 3);

        int offset = 0;
        Assert.Equal(ProtocolConstants.MsgUpsert, buf[offset++]);

        uint nameLen = BinaryPrimitives.ReadUInt32BigEndian(buf.AsSpan(offset));
        offset += 4;
        offset += (int)nameLen;

        Assert.Equal(ProtocolConstants.ActionDelete, buf[offset++]);

        uint docCount = BinaryPrimitives.ReadUInt32BigEndian(buf.AsSpan(offset));
        Assert.Equal(3u, docCount);

        Assert.Equal(size, pos);
    }

    [Fact]
    public void EncodeUpsertHeader_Utf8Collection()
    {
        string collection = "produkter_ö";
        int size = MessageEncoder.ComputeUpsertHeaderSize(collection);
        var buf = new byte[size];
        int pos = 0;
        MessageEncoder.EncodeUpsertHeader(buf, ref pos, collection, ProtocolConstants.ActionUpsert, 1);

        uint nameLen = BinaryPrimitives.ReadUInt32BigEndian(buf.AsSpan(1));
        string name = Encoding.UTF8.GetString(buf, 5, (int)nameLen);
        Assert.Equal(collection, name);
        Assert.Equal(size, pos);
    }

    [Fact]
    public void EncodeDeleteIds_MultipleIds()
    {
        var ids = new[] { "doc-1", "doc-2", "doc-3" };
        int size = MessageEncoder.ComputeDeleteIdsSize(ids);
        var buf = new byte[size];
        int pos = 0;
        MessageEncoder.EncodeDeleteIds(buf, ref pos, ids);

        int offset = 0;
        foreach (var id in ids)
        {
            ushort idLen = BinaryPrimitives.ReadUInt16BigEndian(buf.AsSpan(offset));
            offset += 2;
            Assert.Equal(id.Length, idLen);
            string decoded = Encoding.UTF8.GetString(buf, offset, idLen);
            Assert.Equal(id, decoded);
            offset += idLen;
        }

        Assert.Equal(size, pos);
        Assert.Equal(size, offset);
    }

    [Fact]
    public void EncodeDeleteIds_Utf8Ids()
    {
        var ids = new[] { "dok-ä1", "dok-ö2" };
        int size = MessageEncoder.ComputeDeleteIdsSize(ids);
        var buf = new byte[size];
        int pos = 0;
        MessageEncoder.EncodeDeleteIds(buf, ref pos, ids);

        int offset = 0;
        foreach (var id in ids)
        {
            int expectedLen = Encoding.UTF8.GetByteCount(id);
            ushort idLen = BinaryPrimitives.ReadUInt16BigEndian(buf.AsSpan(offset));
            offset += 2;
            Assert.Equal(expectedLen, idLen);
            string decoded = Encoding.UTF8.GetString(buf, offset, idLen);
            Assert.Equal(id, decoded);
            offset += idLen;
        }

        Assert.Equal(size, pos);
    }
}
