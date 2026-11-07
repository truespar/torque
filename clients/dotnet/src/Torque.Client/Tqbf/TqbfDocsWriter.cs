using System.Buffers.Binary;
using System.IO.Compression;
using System.IO.Hashing;
using System.Text;
using System.Text.Json;

namespace Torque.Http.Tqbf;

/// <summary>
/// Writes a docs-only TQBF file (spec docs/tqbf-segment-format.md §10): the collection schema
/// (SCHEMA), every document as a binary record (RECORDS, zlib chunks) and the registry of document
/// paths (FIELDS). <c>torque-admin build</c> turns it into segment files, which
/// <see cref="Collections.CollectionClient.UploadSegmentsAsync"/> uploads; a server can also build
/// the docs-only file itself. Documents keep the order they are added in: that is their doc id
/// order, which decides tie order in results. Streams to disk; holds one chunk of records.
/// </summary>
/// <example>
/// <code>
/// using var w = new TqbfDocsWriter("companies.tqbf", schemaJson);
/// foreach (var line in jsonl) w.AddJson(line);
/// var doc = w.StartDocument();
/// doc.Property("id").String("42").Property("name").String("Acme");
/// w.EndDocument();
/// w.Complete();
/// </code>
/// </example>
public sealed class TqbfDocsWriter : IDisposable
{
    private const int HeaderLen = 64;
    private const int EntryLen = 40;
    private const int ChunkTarget = 1 << 20;
    private const ushort SegmentWide = 0xFFFF;
    private const ushort KindFields = 1, KindSchema = 4, KindRecords = 5;

    private readonly FileStream _file;
    private readonly List<(ushort Kind, long Offset, long Length, ulong Xxh3)> _table = new();
    private readonly TqbfFieldRegistry _registry = new();
    private readonly TqbfDocument _doc;
    private readonly MemoryStream _chunk = new();
    private readonly CompressionLevel _level;
    private int _chunkDocs;
    private readonly List<long> _chunkOffsets = new();
    private readonly long _recordsStart;
    private readonly XxHash3 _recordsHash = new();
    private uint _docCount;
    private bool _inDocument;
    private bool _complete;

    /// <summary>Documents added so far.</summary>
    public uint DocumentCount => _docCount;

    /// <param name="path">Output file; replaced when it exists.</param>
    /// <param name="schemaJson">The collection schema, as <c>POST /collections</c> takes it.</param>
    /// <param name="level">zlib level of the record chunks (the file only lives until the build).</param>
    public TqbfDocsWriter(string path, string schemaJson, CompressionLevel level = CompressionLevel.Fastest)
    {
        ArgumentNullException.ThrowIfNull(path);
        ArgumentNullException.ThrowIfNull(schemaJson);
        using (var schema = JsonDocument.Parse(schemaJson))
        {
            if (schema.RootElement.ValueKind != JsonValueKind.Object || !schema.RootElement.TryGetProperty("fields", out _))
                throw new ArgumentException("The schema must be a collection schema object with `fields`.", nameof(schemaJson));
        }
        _level = level;
        _doc = new TqbfDocument(_registry);
        _file = new FileStream(path, FileMode.Create, FileAccess.ReadWrite, FileShare.None, 1 << 20);
        _file.Write(new byte[HeaderLen]);
        WriteSection(KindSchema, Encoding.UTF8.GetBytes(schemaJson));
        Align(4096);
        _recordsStart = _file.Position;
    }

    /// <summary>Add one document given as UTF-8 JSON (a line of a Typesense JSONL import).</summary>
    public void AddJson(ReadOnlySpan<byte> utf8Json)
    {
        BeginRecord();
        try
        {
            _doc.ReadJson(utf8Json);
        }
        catch
        {
            _inDocument = false;
            throw;
        }
        EndDocument();
    }

    /// <summary>Add one document given as JSON text.</summary>
    public void AddJson(string json) => AddJson(Encoding.UTF8.GetBytes(json));

    /// <summary>Start a document built from value events; finish it with <see cref="EndDocument"/>.</summary>
    public TqbfDocument StartDocument()
    {
        BeginRecord();
        return _doc;
    }

    private void BeginRecord()
    {
        if (_complete) throw new InvalidOperationException("The file is complete.");
        if (_inDocument) throw new InvalidOperationException("The previous document was not ended.");
        if (_docCount == uint.MaxValue) throw new InvalidOperationException("A TQBF file holds at most 2^32 - 1 documents.");
        _doc.Reset();
        _inDocument = true;
    }

    /// <summary>Encode the current document and append it.</summary>
    public void EndDocument()
    {
        if (!_inDocument) throw new InvalidOperationException("No document was started.");
        _inDocument = false;
        var rec = _doc.Encode();
        Span<byte> len = stackalloc byte[10];
        int n = 0;
        ulong v = (ulong)rec.Length;
        while (v >= 0x80) { len[n++] = (byte)(v | 0x80); v >>= 7; }
        len[n++] = (byte)v;
        _chunk.Write(len[..n]);
        _chunk.Write(rec);
        _chunkDocs++;
        _docCount++;
        if (_chunk.Length >= ChunkTarget)
            FlushChunk();
    }

    private void FlushChunk()
    {
        if (_chunkDocs == 0) return;
        _chunkOffsets.Add(_file.Position - _recordsStart);
        Span<byte> head = stackalloc byte[8];
        BinaryPrimitives.WriteUInt32LittleEndian(head, (uint)_chunkDocs);
        BinaryPrimitives.WriteUInt32LittleEndian(head[4..], (uint)_chunk.Length);
        RecordsWrite(head);
        var packed = new MemoryStream((int)(_chunk.Length / 2));
        using (var z = new ZLibStream(packed, _level, leaveOpen: true))
            z.Write(_chunk.GetBuffer(), 0, (int)_chunk.Length);
        RecordsWrite(packed.GetBuffer().AsSpan(0, (int)packed.Length));
        _chunk.SetLength(0);
        _chunkDocs = 0;
    }

    private void RecordsWrite(ReadOnlySpan<byte> bytes)
    {
        _file.Write(bytes);
        _recordsHash.Append(bytes);
    }

    /// <summary>Finish the file: the last chunk, FIELDS, the section table and the header.</summary>
    public void Complete()
    {
        if (_complete) return;
        if (_inDocument) throw new InvalidOperationException("The last document was not ended.");
        FlushChunk();
        var trailer = new byte[8 * (_chunkOffsets.Count + 1) + 8];
        for (int i = 0; i < _chunkOffsets.Count; i++)
            BinaryPrimitives.WriteInt64LittleEndian(trailer.AsSpan(8 * i), _chunkOffsets[i]);
        long trailerAt = _file.Position - _recordsStart;
        BinaryPrimitives.WriteInt64LittleEndian(trailer.AsSpan(8 * _chunkOffsets.Count), trailerAt);
        BinaryPrimitives.WriteUInt32LittleEndian(trailer.AsSpan(trailer.Length - 8), _docCount);
        BinaryPrimitives.WriteUInt32LittleEndian(trailer.AsSpan(trailer.Length - 4), (uint)_chunkOffsets.Count);
        RecordsWrite(trailer);
        _table.Add((KindRecords, _recordsStart, _file.Position - _recordsStart, _recordsHash.GetCurrentHashAsUInt64()));

        byte[] fields = _registry.Encode();
        WriteSection(KindFields, fields);

        // Section table (in section order), then the header that points at it.
        Align(8);
        long tableAt = _file.Position;
        var table = new byte[_table.Count * EntryLen];
        var ordered = _table.OrderBy(e => e.Offset).ToList();
        for (int i = 0; i < ordered.Count; i++)
        {
            var t = table.AsSpan(i * EntryLen, EntryLen);
            var e = ordered[i];
            BinaryPrimitives.WriteUInt16LittleEndian(t, e.Kind);
            BinaryPrimitives.WriteUInt16LittleEndian(t[2..], SegmentWide);
            t[4] = 0; // codec: stored as is
            BinaryPrimitives.WriteInt64LittleEndian(t[8..], e.Offset);
            BinaryPrimitives.WriteInt64LittleEndian(t[16..], e.Length);
            BinaryPrimitives.WriteInt64LittleEndian(t[24..], e.Length);
            BinaryPrimitives.WriteUInt64LittleEndian(t[32..], e.Xxh3);
        }
        _file.Write(table);

        var header = new byte[HeaderLen];
        "TQBF"u8.CopyTo(header);
        BinaryPrimitives.WriteUInt16LittleEndian(header.AsSpan(4), 6);
        BinaryPrimitives.WriteUInt16LittleEndian(header.AsSpan(6), 0); // flags: no index sections
        BinaryPrimitives.WriteUInt32LittleEndian(header.AsSpan(8), _docCount);
        BinaryPrimitives.WriteUInt32LittleEndian(header.AsSpan(12), (uint)_table.Count);
        BinaryPrimitives.WriteUInt64LittleEndian(header.AsSpan(16), XxHash3.HashToUInt64(fields));
        BinaryPrimitives.WriteInt64LittleEndian(header.AsSpan(24), DateTimeOffset.UtcNow.ToUnixTimeSeconds());
        BinaryPrimitives.WriteInt64LittleEndian(header.AsSpan(32), tableAt);
        BinaryPrimitives.WriteUInt64LittleEndian(header.AsSpan(40), XxHash3.HashToUInt64(table));
        _file.Position = 0;
        _file.Write(header);
        _file.Flush(flushToDisk: true);
        _complete = true;
        _file.Dispose();
    }

    private void WriteSection(ushort kind, byte[] bytes)
    {
        Align(bytes.Length > 64 * 1024 ? 4096 : 64);
        long at = _file.Position;
        _file.Write(bytes);
        _table.Add((kind, at, bytes.Length, XxHash3.HashToUInt64(bytes)));
    }

    private void Align(int to)
    {
        long pad = (to - _file.Position % to) % to;
        if (pad > 0) _file.Write(new byte[pad]);
    }

    /// <summary>Closes the file; an incomplete file is left unusable (no header).</summary>
    public void Dispose()
    {
        if (!_complete) _file.Dispose();
    }
}
