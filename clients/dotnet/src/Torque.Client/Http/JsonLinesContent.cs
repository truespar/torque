using System.Net;
using System.Net.Http.Headers;
using System.Text.Json;
using System.Text.Json.Serialization.Metadata;

namespace Torque.Http.Http;

/// <summary>
/// Streams an <see cref="IAsyncEnumerable{T}"/> as newline-delimited JSON directly
/// to the request body without materializing the full payload in memory.
/// </summary>
internal sealed class JsonLinesContent<T>(
    IAsyncEnumerable<T> items,
    JsonTypeInfo<T> typeInfo) : HttpContent
{
    private static readonly byte[] Newline = "\n"u8.ToArray();

    protected override async Task SerializeToStreamAsync(Stream stream, TransportContext? context)
    {
        await foreach (var item in items.ConfigureAwait(false))
        {
            await JsonSerializer.SerializeAsync(stream, item, typeInfo).ConfigureAwait(false);
            await stream.WriteAsync(Newline).ConfigureAwait(false);
        }
    }

    protected override bool TryComputeLength(out long length)
    {
        length = -1;
        return false;
    }

    protected override Task SerializeToStreamAsync(Stream stream, TransportContext? context, CancellationToken ct)
    {
        return SerializeToStreamCoreAsync(stream, ct);
    }

    private async Task SerializeToStreamCoreAsync(Stream stream, CancellationToken ct)
    {
        await foreach (var item in items.WithCancellation(ct).ConfigureAwait(false))
        {
            await JsonSerializer.SerializeAsync(stream, item, typeInfo, ct).ConfigureAwait(false);
            await stream.WriteAsync(Newline, ct).ConfigureAwait(false);
        }
    }
}

/// <summary>
/// Streams an <see cref="IAsyncEnumerable{String}"/> as newline-delimited text directly
/// to the request body. Each string is expected to be a single JSON line.
/// </summary>
internal sealed class StringLinesContent(IAsyncEnumerable<string> lines) : HttpContent
{
    private static readonly byte[] Newline = "\n"u8.ToArray();

    protected override async Task SerializeToStreamAsync(Stream stream, TransportContext? context)
    {
        await foreach (var line in lines.ConfigureAwait(false))
        {
            var bytes = System.Text.Encoding.UTF8.GetBytes(line);
            await stream.WriteAsync(bytes).ConfigureAwait(false);
            await stream.WriteAsync(Newline).ConfigureAwait(false);
        }
    }

    protected override bool TryComputeLength(out long length)
    {
        length = -1;
        return false;
    }

    protected override Task SerializeToStreamAsync(Stream stream, TransportContext? context, CancellationToken ct)
    {
        return SerializeToStreamCoreAsync(stream, ct);
    }

    private async Task SerializeToStreamCoreAsync(Stream stream, CancellationToken ct)
    {
        await foreach (var line in lines.WithCancellation(ct).ConfigureAwait(false))
        {
            var bytes = System.Text.Encoding.UTF8.GetBytes(line);
            await stream.WriteAsync(bytes, ct).ConfigureAwait(false);
            await stream.WriteAsync(Newline, ct).ConfigureAwait(false);
        }
    }
}
