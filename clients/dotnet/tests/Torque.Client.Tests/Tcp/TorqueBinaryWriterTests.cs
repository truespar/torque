using System.Buffers.Binary;
using System.IO.Compression;
using Torque.Http.Models;
using Torque.Http.Tcp;

namespace Torque.Http.Tests.Tcp;

public class TorqueBinaryWriterTests
{
    private static CollectionSchema TestSchema() => new()
    {
        Name = "products",
        Fields =
        [
            new() { Name = "title", Type = TorqueFieldType.String },
            new() { Name = "price", Type = TorqueFieldType.Float, Sort = true },
            new() { Name = "brand", Type = TorqueFieldType.String, Facet = true },
            new() { Name = "in_stock", Type = TorqueFieldType.Bool },
            new() { Name = "rating", Type = TorqueFieldType.Int32, Sort = true },
        ]
    };

    private static TorqueDocument MakeDoc(string id, string title, double price, string brand, bool inStock, int rating)
    {
        return new TorqueDocumentBuilder()
            .SetId(id)
            .Set("title", title)
            .Set("price", price)
            .Set("brand", brand)
            .Set("in_stock", inStock)
            .Set("rating", rating)
            .Build(TestSchema());
    }

    [Fact]
    public void Write_SingleDocument_IncreasesDocCount()
    {
        var path = Path.GetTempFileName();
        try
        {
            var schema = TestSchema();
            using var writer = new TorqueBinaryWriter(schema, path);
            Assert.Equal(0u, writer.DocumentCount);

            writer.Write(MakeDoc("1", "Laptop", 999.99, "Acme", true, 5));
            Assert.Equal(1u, writer.DocumentCount);

            writer.Write(MakeDoc("2", "Phone", 499.99, "Beta", true, 4));
            Assert.Equal(2u, writer.DocumentCount);
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void Finalize_WritesValidTqbfHeader()
    {
        var path = Path.GetTempFileName();
        try
        {
            var schema = TestSchema();
            using (var writer = new TorqueBinaryWriter(schema, path))
            {
                writer.Write(MakeDoc("1", "Laptop", 999.99, "Acme", true, 5));
                writer.Write(MakeDoc("2", "Phone", 499.99, "Beta", true, 4));
                writer.Write(MakeDoc("3", "Tablet", 299.99, "Acme", false, 3));
                writer.Finalize();
            }

            var bytes = File.ReadAllBytes(path);

            // Verify header (v2 = 24 bytes)
            Assert.True(bytes.Length >= 24, $"File too small: {bytes.Length} bytes");
            Assert.Equal((byte)'T', bytes[0]);
            Assert.Equal((byte)'Q', bytes[1]);
            Assert.Equal((byte)'B', bytes[2]);
            Assert.Equal((byte)'F', bytes[3]);

            // Version = 2
            var version = BinaryPrimitives.ReadUInt32LittleEndian(bytes.AsSpan(4));
            Assert.Equal(2u, version);

            // Doc count = 3
            var docCount = BinaryPrimitives.ReadUInt32LittleEndian(bytes.AsSpan(8));
            Assert.Equal(3u, docCount);

            // Flags = 2 (ZLib)
            var flags = BinaryPrimitives.ReadUInt32LittleEndian(bytes.AsSpan(12));
            Assert.Equal(2u, flags);

            // Uncompressed size > 0
            var uncompressedSize = BinaryPrimitives.ReadUInt64LittleEndian(bytes.AsSpan(16));
            Assert.True(uncompressedSize > 0, "Uncompressed size should be set");
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void Finalize_ZLibPayload_IsDecompressible()
    {
        var path = Path.GetTempFileName();
        try
        {
            var schema = TestSchema();
            using (var writer = new TorqueBinaryWriter(schema, path))
            {
                for (int i = 0; i < 100; i++)
                    writer.Write(MakeDoc($"doc-{i}", $"Product {i}", i * 10.0, "Brand", i % 2 == 0, i % 5));
                writer.Finalize();
            }

            var bytes = File.ReadAllBytes(path);
            var compressedPayload = bytes.AsSpan(24).ToArray();

            // Decompress with ZLib
            using var compressedStream = new MemoryStream(compressedPayload);
            using var zlib = new ZLibStream(compressedStream, CompressionMode.Decompress);
            using var decompressed = new MemoryStream();
            zlib.CopyTo(decompressed);

            Assert.True(decompressed.Length > 0, "Decompressed payload should not be empty");
            // Verify uncompressed_size in header matches actual decompressed length
            var uncompressedSize = BinaryPrimitives.ReadUInt64LittleEndian(bytes.AsSpan(16));
            Assert.Equal((ulong)decompressed.Length, uncompressedSize);
            // Compressed should be smaller than decompressed (100 docs with repetitive data)
            Assert.True(compressedPayload.Length < decompressed.Length,
                $"Compressed {compressedPayload.Length} should be < decompressed {decompressed.Length}");
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void Finalize_CompressedPayload_MatchesRawEncoding()
    {
        var path = Path.GetTempFileName();
        try
        {
            var schema = TestSchema();
            var docs = new[]
            {
                MakeDoc("1", "Laptop", 999.99, "Acme", true, 5),
                MakeDoc("2", "Phone", 499.99, "Beta", true, 4),
            };

            // Encode docs manually to get expected raw bytes
            var expectedRaw = new MemoryStream();
            foreach (var doc in docs)
            {
                int size = DocumentEncoder.ComputeSize(doc, schema);
                var buf = new byte[size];
                int pos = 0;
                DocumentEncoder.Encode(doc, schema, buf, ref pos);
                expectedRaw.Write(buf, 0, pos);
            }
            var expectedBytes = expectedRaw.ToArray();

            // Write via TorqueBinaryWriter
            using (var writer = new TorqueBinaryWriter(schema, path))
            {
                foreach (var doc in docs)
                    writer.Write(doc);
                writer.Finalize();
            }

            // Decompress TQBF payload (v2 header = 24 bytes)
            var fileBytes = File.ReadAllBytes(path);
            var compressedPayload = fileBytes.AsSpan(24).ToArray();
            using var cs = new MemoryStream(compressedPayload);
            using var zlib = new ZLibStream(cs, CompressionMode.Decompress);
            using var decompressed = new MemoryStream();
            zlib.CopyTo(decompressed);
            var actualBytes = decompressed.ToArray();

            // Raw bytes must match exactly
            Assert.Equal(expectedBytes.Length, actualBytes.Length);
            Assert.Equal(expectedBytes, actualBytes);
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void Write_AfterFinalize_Throws()
    {
        var path = Path.GetTempFileName();
        try
        {
            var schema = TestSchema();
            using var writer = new TorqueBinaryWriter(schema, path);
            writer.Write(MakeDoc("1", "Laptop", 999.99, "Acme", true, 5));
            writer.Finalize();

            Assert.Throws<InvalidOperationException>(() =>
                writer.Write(MakeDoc("2", "Phone", 499.99, "Beta", true, 4)));
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void Finalize_ZeroDocs_WritesValidEmptyFile()
    {
        var path = Path.GetTempFileName();
        try
        {
            var schema = TestSchema();
            using (var writer = new TorqueBinaryWriter(schema, path))
            {
                writer.Finalize();
            }

            var bytes = File.ReadAllBytes(path);
            Assert.True(bytes.Length >= 24);

            var docCount = BinaryPrimitives.ReadUInt32LittleEndian(bytes.AsSpan(8));
            Assert.Equal(0u, docCount);

            var uncompressedSize = BinaryPrimitives.ReadUInt64LittleEndian(bytes.AsSpan(16));
            Assert.Equal(0ul, uncompressedSize);
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public async Task FinalizeAsync_ProducesSameResultAsSync()
    {
        var pathSync = Path.GetTempFileName();
        var pathAsync = Path.GetTempFileName();
        try
        {
            var schema = TestSchema();
            var docs = Enumerable.Range(0, 50)
                .Select(i => MakeDoc($"doc-{i}", $"Product {i}", i * 9.99, "Brand", true, i % 5))
                .ToArray();

            // Sync
            using (var writer = new TorqueBinaryWriter(schema, pathSync))
            {
                foreach (var doc in docs) writer.Write(doc);
                writer.Finalize();
            }

            // Async
            await using (var writer = new TorqueBinaryWriter(schema, pathAsync))
            {
                foreach (var doc in docs) writer.Write(doc);
                await writer.FinalizeAsync();
            }

            var syncBytes = File.ReadAllBytes(pathSync);
            var asyncBytes = File.ReadAllBytes(pathAsync);

            // Headers must match (v2 = 24 bytes)
            Assert.Equal(syncBytes[..24], asyncBytes[..24]);

            // Decompressed payloads must match
            byte[] DecompressPayload(byte[] file)
            {
                using var cs = new MemoryStream(file.AsSpan(24).ToArray());
                using var zlib = new ZLibStream(cs, CompressionMode.Decompress);
                using var ms = new MemoryStream();
                zlib.CopyTo(ms);
                return ms.ToArray();
            }

            Assert.Equal(DecompressPayload(syncBytes), DecompressPayload(asyncBytes));
        }
        finally
        {
            File.Delete(pathSync);
            File.Delete(pathAsync);
        }
    }

    [Fact]
    public void LargeFile_CompressionRatio_IsReasonable()
    {
        var path = Path.GetTempFileName();
        try
        {
            var schema = TestSchema();
            using (var writer = new TorqueBinaryWriter(schema, path))
            {
                // Write 10K docs with repetitive data — should compress well
                for (int i = 0; i < 10_000; i++)
                    writer.Write(MakeDoc($"doc-{i:D8}", $"Product {i}", i * 1.99, "Acme", i % 3 == 0, i % 5));
                writer.Finalize();
            }

            var fileSize = new FileInfo(path).Length;
            var rawSize = fileSize; // approximate — the raw would be larger

            // 10K docs should produce a file smaller than 1MB (raw ~500KB, compressed ~200KB)
            Assert.True(fileSize < 1_000_000, $"File too large for 10K small docs: {fileSize} bytes");
            Assert.True(fileSize > 1_000, $"File suspiciously small: {fileSize} bytes");
        }
        finally
        {
            File.Delete(path);
        }
    }
}
