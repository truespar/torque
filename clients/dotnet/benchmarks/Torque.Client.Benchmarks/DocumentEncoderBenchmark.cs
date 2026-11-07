using System.Buffers;
using BenchmarkDotNet.Attributes;
using Torque.Http.Models;
using Torque.Http.Tcp;

namespace Torque.Http.Benchmarks;

[MemoryDiagnoser]
public class DocumentEncoderBenchmark
{
    private CollectionSchema _schema = null!;
    private TorqueDocument[] _docs = null!;
    private byte[] _buf = null!;

    [Params(1000, 10000)]
    public int DocCount { get; set; }

    [GlobalSetup]
    public void Setup()
    {
        _schema = new CollectionSchema
        {
            Name = "bench",
            Fields =
            [
                new() { Name = "title", Type = TorqueFieldType.String },
                new() { Name = "price", Type = TorqueFieldType.Int32, Sort = true },
                new() { Name = "rating", Type = TorqueFieldType.Float },
                new() { Name = "in_stock", Type = TorqueFieldType.Bool },
                new() { Name = "tags", Type = TorqueFieldType.StringArray, Facet = true },
            ]
        };

        _docs = new TorqueDocument[DocCount];
        for (int i = 0; i < DocCount; i++)
        {
            _docs[i] = new TorqueDocumentBuilder()
                .SetId($"doc-{i}")
                .Set("title", $"Product number {i} with some description text")
                .Set("price", 100 + i)
                .Set("rating", 3.5 + (i % 20) * 0.1)
                .Set("in_stock", i % 3 != 0)
                .Set("tags", new[] { "tag1", "tag2", "tag3" })
                .Build(_schema);
        }

        // Pre-allocate a reusable buffer (like TorqueIngestClient does with ArrayPool)
        int totalSize = 0;
        for (int i = 0; i < DocCount; i++)
            totalSize += DocumentEncoder.ComputeSize(_docs[i], _schema);
        _buf = ArrayPool<byte>.Shared.Rent(totalSize + 1024);
    }

    [GlobalCleanup]
    public void Cleanup()
    {
        ArrayPool<byte>.Shared.Return(_buf);
    }

    [Benchmark]
    public int EncodeSingleDocument()
    {
        int pos = 0;
        DocumentEncoder.Encode(_docs[0], _schema, _buf, ref pos);
        return pos;
    }

    [Benchmark]
    public int EncodeBatch()
    {
        int pos = 0;
        for (int i = 0; i < _docs.Length; i++)
            DocumentEncoder.Encode(_docs[i], _schema, _buf, ref pos);
        return pos;
    }
}
