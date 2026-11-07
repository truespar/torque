using System.Collections.Concurrent;
using System.Text;
using System.Text.Json;

namespace Torque.Http.Tqbf;

/// <summary>Options of a <see cref="TqbfSegmentBuilder"/>.</summary>
public sealed record TqbfSegmentBuildOptions
{
    /// <summary>zstd level of the stored documents (19: smallest, compressed while records arrive).</summary>
    public int DocsLevel { get; init; } = 19;

    /// <summary>Threads the final merge uses; 0 = every core.</summary>
    public int Threads { get; init; }

    /// <summary>Accumulator bytes after which a run freezes a piece; 0 = the builder's default (64 MB).</summary>
    public long PieceBytes { get; init; }

    /// <summary>Frozen pieces go to a spill file (mapped for the merge), so memory does not grow
    /// with the collection; false keeps them in memory.</summary>
    public bool SpillToDisk { get; init; } = true;

    /// <summary>Directory of the spill file; null = the system temp directory.</summary>
    public string? SpillDirectory { get; init; }
}

/// <summary>A document the builder refused, with the message a JSONL import of it gives.</summary>
public sealed record TqbfRejected(long Run, long Index, string Id, string Error);

/// <summary>What <see cref="TqbfSegmentBuilder.Finish"/> wrote.</summary>
public sealed record TqbfSegmentReport(uint Documents, int Pieces, long Bytes, long FrozenBytes, long MergeMilliseconds, IReadOnlyList<TqbfRejected> Rejected);

/// <summary>
/// Builds a complete, ready-to-map segment file (spec §10.2) in this process, with the native
/// builder that shares the server's indexing code: documents go in as binary records, nothing is
/// serialized as JSON, and the server installs the uploaded file without decoding anything.
/// </summary>
/// <remarks>
/// Documents are added in runs (<see cref="BeginRun"/>): each run is a stretch of consecutive doc
/// ids and belongs to one thread, which indexes its records as it adds them. Runs are laid out in
/// key order, so give each run the key of the data it covers (e.g. the index of its id range); the
/// thread timing then never changes the file. With keys 0, 1, 2, ... the stored documents are
/// compressed while they arrive (the dictionary is trained on the first 8 MB of records in key
/// order). <see cref="Finish"/> merges the ended runs in parallel and writes the file. Document
/// ids must be unique within a build.
/// </remarks>
/// <example>
/// <code>
/// using var b = new TqbfSegmentBuilder(schemaJson);
/// Parallel.For(0, ranges.Length, i =>
/// {
///     using var run = b.BeginRun(i);
///     foreach (var json in Load(ranges[i])) run.AddJson(json);
///     run.End();
/// });
/// var report = b.Finish("companies.tqbf");
/// </code>
/// </example>
public sealed unsafe class TqbfSegmentBuilder : IDisposable, ITqbfPaths
{
    private nint _handle;
    private readonly ConcurrentDictionary<(ushort Parent, string Name), ushort> _ids = new();
    private readonly ConcurrentDictionary<ushort, byte[]> _names = new();
    private int _openRuns;

    /// <param name="schemaJson">The collection schema, as <c>POST /collections</c> takes it.</param>
    /// <param name="options">Build options; null for the defaults.</param>
    public TqbfSegmentBuilder(string schemaJson, TqbfSegmentBuildOptions? options = null)
    {
        ArgumentNullException.ThrowIfNull(schemaJson);
        options ??= new TqbfSegmentBuildOptions();
        byte[] schema = Encoding.UTF8.GetBytes(schemaJson);
        byte[]? spillDir = options.SpillDirectory is null ? null : Encoding.UTF8.GetBytes(Path.GetFullPath(options.SpillDirectory));
        fixed (byte* s = schema)
        fixed (byte* d = spillDir)
        {
            var o = new TqbfNative.Options
            {
                DocsLevel = options.DocsLevel,
                Threads = (uint)Math.Max(0, options.Threads),
                PieceBytes = (ulong)Math.Max(0, options.PieceBytes),
                Spill = options.SpillToDisk ? 1u : 0u,
                SpillDir = d,
                SpillDirLen = (nuint)(spillDir?.Length ?? 0),
            };
            _handle = TqbfNative.tqb_new(s, (nuint)schema.Length, &o);
        }
        if (_handle == 0)
            throw TqbfNative.Error("Invalid schema");
    }

    internal nint Handle => _handle != 0 ? _handle : throw new ObjectDisposedException(nameof(TqbfSegmentBuilder));

    /// <summary>Start run <paramref name="key"/> (unique; runs are laid out in key order).</summary>
    public TqbfRun BeginRun(long key)
    {
        nint r = TqbfNative.tqb_run_begin(Handle, (ulong)key);
        if (r == 0)
            throw TqbfNative.Error("Could not start a run");
        Interlocked.Increment(ref _openRuns);
        return new TqbfRun(this, r, key);
    }

    internal void RunClosed() => Interlocked.Decrement(ref _openRuns);

    /// <summary>Merge every ended run and write the segment file. The builder is used up.</summary>
    /// <exception cref="InvalidOperationException">A run is still open.</exception>
    public TqbfSegmentReport Finish(string path)
    {
        ArgumentNullException.ThrowIfNull(path);
        if (Volatile.Read(ref _openRuns) != 0)
            throw new InvalidOperationException("Every run must be ended before the segment is finished.");
        nint h = Handle;
        _handle = 0;
        byte[] p = Encoding.UTF8.GetBytes(Path.GetFullPath(path));
        byte* json;
        nuint len;
        int rc;
        fixed (byte* pp = p)
            rc = TqbfNative.tqb_finish(h, pp, (nuint)p.Length, &json, &len);
        if (rc != 0)
            throw TqbfNative.Error("Segment build failed");
        using var doc = JsonDocument.Parse(TqbfNative.Take(json, len));
        var r = doc.RootElement;
        return new TqbfSegmentReport(
            r.GetProperty("docs").GetUInt32(),
            r.GetProperty("pieces").GetInt32(),
            r.GetProperty("bytes").GetInt64(),
            r.GetProperty("frozen_bytes").GetInt64(),
            r.GetProperty("merge_millis").GetInt64(),
            ParseRejected(r.GetProperty("rejected")));
    }

    internal static IReadOnlyList<TqbfRejected> ParseRejected(JsonElement a)
    {
        var list = new List<TqbfRejected>(a.GetArrayLength());
        foreach (var e in a.EnumerateArray())
            list.Add(new TqbfRejected(e.GetProperty("run").GetInt64(), e.GetProperty("index").GetInt64(),
                e.GetProperty("id").GetString()!, e.GetProperty("error").GetString()!));
        return list;
    }

    ushort ITqbfPaths.Id(ushort parent, string name)
    {
        if (_ids.TryGetValue((parent, name), out ushort id))
            return id;
        byte[] utf8 = Encoding.UTF8.GetBytes(name);
        int rc;
        fixed (byte* n = utf8)
            rc = TqbfNative.tqb_path_id(Handle, parent, n, (nuint)utf8.Length, &id);
        if (rc != 0)
            throw TqbfNative.Error($"Key `{name}`");
        _names.TryAdd(id, utf8);
        _ids.TryAdd((parent, name), id);
        return id;
    }

    ReadOnlySpan<byte> ITqbfPaths.Name(ushort id) => _names[id];

    /// <summary>Drop the builder without writing a file.</summary>
    public void Dispose()
    {
        nint h = Interlocked.Exchange(ref _handle, 0);
        if (h != 0)
            TqbfNative.tqb_free(h);
    }
}

/// <summary>
/// One run of a <see cref="TqbfSegmentBuilder"/>: documents with consecutive doc ids, added on one
/// thread (not thread-safe). Records are handed to the native builder in batches and indexed on
/// the calling thread. <see cref="End"/> it; a disposed run that was not ended is left out.
/// </summary>
public sealed unsafe class TqbfRun : IDisposable
{
    private const int BatchTarget = 512 * 1024;

    private readonly TqbfSegmentBuilder _builder;
    private nint _handle;
    private readonly TqbfDocument _doc;
    private bool _inDocument;
    private byte[] _batch = new byte[BatchTarget + 64 * 1024];
    private int _batchLen;

    internal TqbfRun(TqbfSegmentBuilder builder, nint handle, long key)
    {
        _builder = builder;
        _handle = handle;
        _doc = new TqbfDocument(builder);
        Key = key;
    }

    /// <summary>The run's key.</summary>
    public long Key { get; }

    /// <summary>The path ids records of this run are encoded with.</summary>
    internal ITqbfPaths Paths => _builder;

    /// <summary>Writer for records encoded straight from objects (<see cref="AddWritten"/> adds them).</summary>
    internal TqbfRecordWriter Writer { get; } = new();

    private nint Handle => _handle != 0 ? _handle : throw new InvalidOperationException("The run has ended.");

    /// <summary>Add one document given as UTF-8 JSON, read as Torque's JSON import reads it.</summary>
    public void AddJson(ReadOnlySpan<byte> utf8Json)
    {
        BeginDocument();
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
        BeginDocument();
        return _doc;
    }

    private void BeginDocument()
    {
        _ = Handle;
        if (_inDocument) throw new InvalidOperationException("The previous document was not ended.");
        _doc.Reset();
        _inDocument = true;
    }

    /// <summary>Encode the current document and add it.</summary>
    public void EndDocument()
    {
        if (!_inDocument) throw new InvalidOperationException("No document was started.");
        _inDocument = false;
        AddRecord(_doc.Encode());
    }

    /// <summary>Add the record <see cref="Writer"/> holds.</summary>
    internal void AddWritten() => AddRecord(Writer.Written);

    private void AddRecord(ReadOnlySpan<byte> rec)
    {
        int need = _batchLen + 10 + rec.Length;
        if (need > _batch.Length)
        {
            Flush();
            if (10 + rec.Length > _batch.Length)
                Array.Resize(ref _batch, 10 + rec.Length);
        }
        ulong v = (ulong)rec.Length;
        while (v >= 0x80)
        {
            _batch[_batchLen++] = (byte)(v | 0x80);
            v >>= 7;
        }
        _batch[_batchLen++] = (byte)v;
        rec.CopyTo(_batch.AsSpan(_batchLen));
        _batchLen += rec.Length;
        if (_batchLen >= BatchTarget)
            Flush();
    }

    private void Flush()
    {
        if (_batchLen == 0) return;
        int rc;
        fixed (byte* p = _batch)
            rc = TqbfNative.tqb_run_add(Handle, p, (nuint)_batchLen);
        _batchLen = 0;
        if (rc != 0)
        {
            // A failed batch may be partly indexed: the run is dropped.
            var e = TqbfNative.Error($"Run {Key}");
            Dispose();
            throw e;
        }
    }

    /// <summary>Documents refused so far (each with the message a JSONL import gives).</summary>
    public long RejectedCount
    {
        get
        {
            Flush();
            return (long)TqbfNative.tqb_run_rejected(Handle);
        }
    }

    /// <summary>Index what is left and hand the run to the builder; returns the refused documents.</summary>
    public IReadOnlyList<TqbfRejected> End()
    {
        if (_inDocument) throw new InvalidOperationException("The last document was not ended.");
        Flush();
        nint h = Handle;
        _handle = 0;
        _builder.RunClosed();
        byte* json;
        nuint len;
        if (TqbfNative.tqb_run_end(h, &json, &len) != 0)
            throw TqbfNative.Error($"Run {Key}");
        using var doc = JsonDocument.Parse(TqbfNative.Take(json, len));
        return TqbfSegmentBuilder.ParseRejected(doc.RootElement);
    }

    /// <summary>Drop a run that was not ended; its documents are left out of the segment.</summary>
    public void Dispose()
    {
        nint h = _handle;
        if (h == 0) return;
        _handle = 0;
        _builder.RunClosed();
        TqbfNative.tqb_run_free(h);
    }
}
