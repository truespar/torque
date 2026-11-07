using System.Buffers;
using System.Buffers.Binary;
using BenchmarkDotNet.Attributes;
using Torque.Http.Models;
using Torque.Http.Tcp;

namespace Torque.Http.Benchmarks;

[MemoryDiagnoser]
public class IngestPipelineBenchmark
{
    private CollectionSchema _schema = null!;
    private TorqueDocument[] _docs = null!;
    private byte[] _sendBuf = null!;

    [GlobalSetup]
    public void Setup()
    {
        _schema = new CollectionSchema
        {
            Name = "pipeline",
            Fields =
            [
                new() { Name = "title", Type = TorqueFieldType.String },
                new() { Name = "body", Type = TorqueFieldType.String },
                new() { Name = "score", Type = TorqueFieldType.Int32, Sort = true },
            ]
        };

        // Pre-build documents (not part of the benchmark)
        _docs = new TorqueDocument[100];
        for (int i = 0; i < 100; i++)
        {
            _docs[i] = new TorqueDocumentBuilder()
                .SetId($"id-{i}")
                .Set("title", $"Title {i}")
                .Set("body", $"This is the body text for document {i} with some content to make it realistic")
                .Set("score", i * 10)
                .Build(_schema);
        }

        // Pre-allocate send buffer (like TorqueIngestClient does)
        _sendBuf = ArrayPool<byte>.Shared.Rent(64 * 1024);
    }

    [GlobalCleanup]
    public void Cleanup()
    {
        ArrayPool<byte>.Shared.Return(_sendBuf);
    }

    [Benchmark]
    public int FullPipeline_100Docs()
    {
        // Simulates TorqueIngestClient.SendBatchAsync hot path:
        // compose full frame in _sendBuf with zero intermediate allocations
        int pos = 4; // reserve frame length prefix

        // DocBatch header
        MessageEncoder.EncodeDocBatchHeader(_sendBuf, ref pos, (uint)_docs.Length);

        // Encode all documents directly into send buffer
        for (int i = 0; i < _docs.Length; i++)
        {
            int docSize = DocumentEncoder.ComputeSize(_docs[i], _schema);
            // In production, EnsureSendCapacity(pos + docSize) would grow if needed
            DocumentEncoder.Encode(_docs[i], _schema, _sendBuf, ref pos);
        }

        // Backfill frame length
        BinaryPrimitives.WriteUInt32BigEndian(_sendBuf.AsSpan(0), (uint)(pos - 4));

        return pos; // return to prevent dead code elimination
    }
}
