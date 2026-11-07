using System.Buffers.Binary;
using System.Text;
using Torque.Http.Exceptions;
using Torque.Http.Models;
using Torque.Http.Tcp;

namespace Torque.Http.Tests.Tcp;

public class MessageDecoderTests
{
    [Fact]
    public void DecodeAck()
    {
        // [0x10][u64 batch_seq=5][u64 docs_received=100]
        var data = new byte[1 + 8 + 8];
        data[0] = ProtocolConstants.MsgAck;
        BinaryPrimitives.WriteUInt64BigEndian(data.AsSpan(1), 5);
        BinaryPrimitives.WriteUInt64BigEndian(data.AsSpan(9), 100);

        var result = MessageDecoder.Decode(data);
        var ack = Assert.IsType<BatchAck>(result);
        Assert.Equal(5UL, ack.BatchSeq);
        Assert.Equal(100UL, ack.DocsReceived);
    }

    [Fact]
    public void DecodeCommitResult_Success()
    {
        // [0x11][u8 success=1][u64 docs_indexed=42][u64 build_ms=150]
        var data = new byte[1 + 1 + 8 + 8];
        data[0] = ProtocolConstants.MsgCommitResult;
        data[1] = 1; // success
        BinaryPrimitives.WriteUInt64BigEndian(data.AsSpan(2), 42);
        BinaryPrimitives.WriteUInt64BigEndian(data.AsSpan(10), 150);

        var result = MessageDecoder.Decode(data);
        var commit = Assert.IsType<CommitResult>(result);
        Assert.True(commit.Success);
        Assert.Equal(42UL, commit.DocsIndexed);
        Assert.Equal(150UL, commit.BuildMs);
    }

    [Fact]
    public void DecodeCommitResult_Failure()
    {
        var data = new byte[1 + 1 + 8 + 8];
        data[0] = ProtocolConstants.MsgCommitResult;
        data[1] = 0; // failure
        BinaryPrimitives.WriteUInt64BigEndian(data.AsSpan(2), 0);
        BinaryPrimitives.WriteUInt64BigEndian(data.AsSpan(10), 0);

        var result = MessageDecoder.Decode(data);
        var commit = Assert.IsType<CommitResult>(result);
        Assert.False(commit.Success);
    }

    [Fact]
    public void DecodeError_ThrowsProtocolException()
    {
        var message = "Collection `foo` not found";
        var msgBytes = Encoding.UTF8.GetBytes(message);

        var data = new byte[1 + 2 + 4 + msgBytes.Length];
        data[0] = ProtocolConstants.MsgError;
        BinaryPrimitives.WriteUInt16BigEndian(data.AsSpan(1), ProtocolConstants.ErrCollectionNotFound);
        BinaryPrimitives.WriteUInt32BigEndian(data.AsSpan(3), (uint)msgBytes.Length);
        msgBytes.CopyTo(data.AsSpan(7));

        var ex = Assert.Throws<TorqueProtocolException>(() => MessageDecoder.Decode(data));
        Assert.Equal(ProtocolConstants.ErrCollectionNotFound, ex.ErrorCode);
        Assert.Equal(message, ex.Message);
    }

    [Fact]
    public void DecodeEmpty_Throws()
    {
        Assert.Throws<TorqueProtocolException>(() => MessageDecoder.Decode([]));
    }

    [Fact]
    public void DecodeUnknownType_Throws()
    {
        Assert.Throws<TorqueProtocolException>(() => MessageDecoder.Decode([0xFF]));
    }

    [Fact]
    public void DecodeBackpressure_ReturnsRetryAfterMs()
    {
        // [0x13][u32 retry_after_ms=1000]
        var data = new byte[1 + 4];
        data[0] = ProtocolConstants.MsgBackpressure;
        BinaryPrimitives.WriteUInt32BigEndian(data.AsSpan(1), 1000);

        var result = MessageDecoder.Decode(data);
        var bp = Assert.IsType<BackpressureResult>(result);
        Assert.Equal(1000u, bp.RetryAfterMs);
    }

    [Fact]
    public void DecodeBackpressure_TooShort_ThrowsProtocolException()
    {
        // Only 3 bytes of payload instead of required 4
        var data = new byte[1 + 3];
        data[0] = ProtocolConstants.MsgBackpressure;

        Assert.Throws<TorqueProtocolException>(() => MessageDecoder.Decode(data));
    }
}
