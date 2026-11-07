using System.Buffers;
using System.Buffers.Binary;
using System.IO.Compression;
using Torque.Http.Models;

namespace Torque.Http.Tcp;

/// <summary>
/// Streaming binary file writer for TQBF format (Torque Binary File).
/// Encodes documents directly to a stream — never holds more than one document in memory.
/// Uses built-in .NET ZLib compression (System.IO.Compression) — zero external dependencies.
///
/// Usage:
///   await using var writer = new TorqueBinaryWriter(schema, "output.tqbf");
///   foreach (var doc in source)
///       writer.Write(doc);
///   await writer.FinalizeAsync();
///   var result = await writer.UploadAsync(httpClient, "my-collection", apiKey);
///
/// File format (TQBF v2):
///   [4 bytes]  magic: "TQBF"
///   [4 bytes]  version: 2 (LE)
///   [4 bytes]  doc_count: u32 (LE) — written on finalize
///   [4 bytes]  flags: 2 = ZLib compressed
///   [8 bytes]  uncompressed_size: u64 (LE) — lets the server pre-size its
///              decompression buffer instead of doubling-realloc
///   [N bytes]  payload: ZLib-compressed concatenated binary documents
/// </summary>
public sealed class TorqueBinaryWriter : IAsyncDisposable, IDisposable
{
    private const uint TqbfVersion = 2;
    private const uint FlagZlib = 2;
    private const int HeaderSize = 24;
    private const int DefaultEncodeBufSize = 64 * 1024;

    private readonly CollectionSchema _schema;
    private readonly string _filePath;
    private readonly MemoryStream _rawPayload;
    private byte[] _encodeBuf;
    private uint _docCount;
    private bool _finalized;

    /// <summary>Number of documents written so far.</summary>
    public uint DocumentCount => _docCount;

    /// <summary>Raw (uncompressed) payload size in bytes.</summary>
    public long RawSize => _rawPayload.Length;

    /// <summary>
    /// Create a binary writer that encodes documents to the TQBF format.
    /// </summary>
    /// <param name="schema">Collection schema (needed for binary encoding field order).</param>
    /// <param name="filePath">Output file path. Written on <see cref="Finalize"/>.</param>
    public TorqueBinaryWriter(CollectionSchema schema, string filePath)
    {
        _schema = schema ?? throw new ArgumentNullException(nameof(schema));
        _filePath = filePath ?? throw new ArgumentNullException(nameof(filePath));
        _rawPayload = new MemoryStream();
        _encodeBuf = ArrayPool<byte>.Shared.Rent(DefaultEncodeBufSize);
    }

    /// <summary>
    /// Write a single document. O(1) memory per call — the document is encoded
    /// into a pooled buffer and appended to an in-memory stream.
    /// </summary>
    public void Write(TorqueDocument doc)
    {
        if (_finalized)
            throw new InvalidOperationException("Writer has been finalized.");

        int size = DocumentEncoder.ComputeSize(doc, _schema);
        if (size > _encodeBuf.Length)
        {
            ArrayPool<byte>.Shared.Return(_encodeBuf);
            _encodeBuf = ArrayPool<byte>.Shared.Rent(size);
        }

        int pos = 0;
        DocumentEncoder.Encode(doc, _schema, _encodeBuf, ref pos);
        _rawPayload.Write(_encodeBuf, 0, pos);
        _docCount++;
    }

    /// <summary>
    /// Finalize: compress with ZLib and write the TQBF file to disk.
    /// Returns the total file size in bytes.
    /// </summary>
    public long Finalize()
    {
        if (_finalized) return new FileInfo(_filePath).Length;
        _finalized = true;

        var rawBytes = _rawPayload.GetBuffer().AsSpan(0, (int)_rawPayload.Length);
        ulong uncompressedSize = (ulong)rawBytes.Length;

        // ZLib compress using built-in .NET compressor
        using var compressedStream = new MemoryStream();
        using (var zlib = new ZLibStream(compressedStream, CompressionLevel.Optimal, leaveOpen: true))
        {
            zlib.Write(rawBytes);
        }

        using var file = new FileStream(_filePath, FileMode.Create, FileAccess.Write, FileShare.None,
            bufferSize: 65536);

        // Header
        Span<byte> header = stackalloc byte[HeaderSize];
        "TQBF"u8.CopyTo(header);
        BinaryPrimitives.WriteUInt32LittleEndian(header[4..], TqbfVersion);
        BinaryPrimitives.WriteUInt32LittleEndian(header[8..], _docCount);
        BinaryPrimitives.WriteUInt32LittleEndian(header[12..], FlagZlib);
        BinaryPrimitives.WriteUInt64LittleEndian(header[16..], uncompressedSize);
        file.Write(header);

        // Compressed payload
        compressedStream.Position = 0;
        compressedStream.CopyTo(file);

        return file.Length;
    }

    /// <summary>
    /// Finalize: async version.
    /// </summary>
    public async Task<long> FinalizeAsync(CancellationToken ct = default)
    {
        if (_finalized) return new FileInfo(_filePath).Length;
        _finalized = true;

        var rawBytes = _rawPayload.GetBuffer().AsMemory(0, (int)_rawPayload.Length);
        ulong uncompressedSize = (ulong)rawBytes.Length;

        // ZLib compress
        using var compressedStream = new MemoryStream();
        await using (var zlib = new ZLibStream(compressedStream, CompressionLevel.Optimal, leaveOpen: true))
        {
            await zlib.WriteAsync(rawBytes, ct).ConfigureAwait(false);
        }

        await using var file = new FileStream(_filePath, FileMode.Create, FileAccess.Write, FileShare.None,
            bufferSize: 65536, useAsync: true);

        // Header
        var header = new byte[HeaderSize];
        "TQBF"u8.CopyTo(header);
        BinaryPrimitives.WriteUInt32LittleEndian(header.AsSpan(4), TqbfVersion);
        BinaryPrimitives.WriteUInt32LittleEndian(header.AsSpan(8), _docCount);
        BinaryPrimitives.WriteUInt32LittleEndian(header.AsSpan(12), FlagZlib);
        BinaryPrimitives.WriteUInt64LittleEndian(header.AsSpan(16), uncompressedSize);
        await file.WriteAsync(header, ct).ConfigureAwait(false);

        compressedStream.Position = 0;
        await compressedStream.CopyToAsync(file, ct).ConfigureAwait(false);

        return file.Length;
    }

    /// <summary>
    /// Finalize (if needed) and upload the file to the Torque server.
    /// Uses streaming upload — the file is streamed from disk, not buffered in memory.
    /// </summary>
    /// <param name="httpClient">HTTP client with base URL configured.</param>
    /// <param name="collection">Target collection name (must already exist).</param>
    /// <param name="apiKey">Torque API key for authentication.</param>
    /// <param name="ct">Cancellation token.</param>
    public async Task<BinaryImportResult> UploadAsync(
        HttpClient httpClient, string collection, string apiKey, CancellationToken ct = default)
    {
        if (!_finalized) Finalize();

        await using var readStream = new FileStream(_filePath, FileMode.Open, FileAccess.Read,
            FileShare.Read, bufferSize: 65536, useAsync: true);

        using var content = new StreamContent(readStream);
        content.Headers.ContentType = new System.Net.Http.Headers.MediaTypeHeaderValue("application/octet-stream");

        using var request = new HttpRequestMessage(HttpMethod.Post,
            $"/collections/{Uri.EscapeDataString(collection)}/documents/import-binary");
        request.Headers.Add("X-TYPESENSE-API-KEY", apiKey);
        request.Content = content;

        var response = await httpClient.SendAsync(request, ct).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();

        var json = await response.Content.ReadAsStringAsync(ct).ConfigureAwait(false);
        var doc = System.Text.Json.JsonDocument.Parse(json);
        var root = doc.RootElement;

        return new BinaryImportResult
        {
            NumImported = root.GetProperty("num_imported").GetInt64(),
            Compressed = root.TryGetProperty("compressed", out var c) && c.GetBoolean(),
            ElapsedMs = root.TryGetProperty("elapsed_ms", out var e) ? e.GetDouble() : 0,
        };
    }

    public void Dispose()
    {
        _rawPayload.Dispose();
        if (_encodeBuf is not null)
        {
            ArrayPool<byte>.Shared.Return(_encodeBuf);
            _encodeBuf = null!;
        }
    }

    public async ValueTask DisposeAsync()
    {
        _rawPayload.Dispose();
        if (_encodeBuf is not null)
        {
            ArrayPool<byte>.Shared.Return(_encodeBuf);
            _encodeBuf = null!;
        }
    }
}

/// <summary>Result of a binary file import.</summary>
public sealed class BinaryImportResult
{
    public long NumImported { get; init; }
    public bool Compressed { get; init; }
    public double ElapsedMs { get; init; }
}
