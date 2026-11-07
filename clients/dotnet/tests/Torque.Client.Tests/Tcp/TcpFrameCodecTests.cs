using System.Buffers.Binary;
using Torque.Http.Tcp;

namespace Torque.Http.Tests.Tcp;

public class TcpFrameCodecTests
{
    /// <summary>
    /// Helper: compose a frame buffer with 4 reserved bytes + payload, then write.
    /// </summary>
    private static async Task WriteTestFrame(Stream stream, byte[] payload)
    {
        var buf = new byte[4 + payload.Length];
        Buffer.BlockCopy(payload, 0, buf, 4, payload.Length);
        await TcpFrameCodec.WriteFrameAsync(stream, buf, buf.Length);
    }

    [Fact]
    public async Task WriteAndReadFrame_Roundtrip()
    {
        byte[] payload = [1, 2, 3, 4, 5];
        using var ms = new MemoryStream();

        await WriteTestFrame(ms, payload);

        ms.Position = 0;
        var recvBuf = new byte[256];
        int len = await TcpFrameCodec.ReadFrameAsync(ms, recvBuf);

        Assert.Equal(payload.Length, len);
        Assert.Equal(payload, recvBuf[..len]);
    }

    [Fact]
    public async Task WriteFrame_LengthPrefix_BigEndian()
    {
        byte[] payload = new byte[300];
        using var ms = new MemoryStream();

        await WriteTestFrame(ms, payload);

        ms.Position = 0;
        var header = new byte[4];
        ms.Read(header, 0, 4);
        uint length = BinaryPrimitives.ReadUInt32BigEndian(header);
        Assert.Equal(300u, length);
    }

    [Fact]
    public async Task ReadFrame_EmptyPayload()
    {
        using var ms = new MemoryStream();
        await WriteTestFrame(ms, []);

        ms.Position = 0;
        var recvBuf = new byte[256];
        int len = await TcpFrameCodec.ReadFrameAsync(ms, recvBuf);
        Assert.Equal(0, len);
    }

    [Fact]
    public async Task ReadFrame_TruncatedStream_Throws()
    {
        using var ms = new MemoryStream([0, 0, 0, 10, 1, 2, 3]); // says 10 bytes but only 3

        var recvBuf = new byte[256];
        await Assert.ThrowsAsync<EndOfStreamException>(() => TcpFrameCodec.ReadFrameAsync(ms, recvBuf));
    }

    [Fact]
    public async Task MultipleFrames_Sequential()
    {
        byte[] payload1 = [10, 20];
        byte[] payload2 = [30, 40, 50];
        using var ms = new MemoryStream();

        await WriteTestFrame(ms, payload1);
        await WriteTestFrame(ms, payload2);

        ms.Position = 0;
        var recvBuf = new byte[256];

        int len1 = await TcpFrameCodec.ReadFrameAsync(ms, recvBuf);
        Assert.Equal(payload1.Length, len1);
        Assert.Equal(payload1, recvBuf[..len1]);

        int len2 = await TcpFrameCodec.ReadFrameAsync(ms, recvBuf);
        Assert.Equal(payload2.Length, len2);
        Assert.Equal(payload2, recvBuf[..len2]);
    }
}
