using System.Buffers.Binary;
using Torque.Http.Exceptions;

namespace Torque.Http.Tcp;

/// <summary>
/// Reads and writes length-prefixed frames over a Stream.
/// Frame format: [u32 BE: payload_length][payload_bytes...]
/// Matches Rust read_frame/write_frame in src/ingest/server.rs.
///
/// Uses caller-owned buffers (ArrayPool-rented) for zero-allocation I/O.
/// </summary>
internal static class TcpFrameCodec
{
    /// <summary>
    /// Write a pre-composed frame. The buffer must have 4 bytes reserved at [0..3]
    /// for the length prefix. Payload starts at offset 4. This method backfills
    /// the length and sends everything in a single WriteAsync call.
    /// </summary>
    public static async Task WriteFrameAsync(Stream stream, byte[] buf, int totalLength, CancellationToken ct = default)
    {
        BinaryPrimitives.WriteUInt32BigEndian(buf.AsSpan(0), (uint)(totalLength - 4));
        await stream.WriteAsync(buf.AsMemory(0, totalLength), ct).ConfigureAwait(false);
        await stream.FlushAsync(ct).ConfigureAwait(false);
    }

    /// <summary>
    /// Read a frame into a caller-owned buffer. Reads the 4-byte length header,
    /// then reads the payload into recvBuf[0..length]. Returns the payload length.
    /// </summary>
    public static async Task<int> ReadFrameAsync(Stream stream, byte[] recvBuf, CancellationToken ct = default)
    {
        // Read 4-byte header into the start of recvBuf
        await ReadExactAsync(stream, recvBuf, 0, 4, ct).ConfigureAwait(false);
        int length = (int)BinaryPrimitives.ReadUInt32BigEndian(recvBuf.AsSpan(0));

        if (length > recvBuf.Length)
            throw new TorqueProtocolException(0, $"Response frame too large: {length} bytes exceeds buffer size {recvBuf.Length}");

        // Read payload into recvBuf[0..length] (overwrites header, which is already parsed)
        if (length > 0)
            await ReadExactAsync(stream, recvBuf, 0, length, ct).ConfigureAwait(false);

        return length;
    }

    internal static async Task ReadExactAsync(Stream stream, byte[] buffer, int offset, int count, CancellationToken ct)
    {
        while (count > 0)
        {
            int read = await stream.ReadAsync(buffer.AsMemory(offset, count), ct).ConfigureAwait(false);
            if (read == 0)
                throw new EndOfStreamException("Connection closed while reading frame");
            offset += read;
            count -= read;
        }
    }
}
