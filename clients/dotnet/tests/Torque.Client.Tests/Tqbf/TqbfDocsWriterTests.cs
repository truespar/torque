using System.Buffers.Binary;
using System.IO.Compression;
using System.IO.Hashing;
using System.Text;
using Torque.Http.Tqbf;

namespace Torque.Http.Tests.Tqbf;

public class TqbfDocsWriterTests
{
    private const string Schema = """{"name":"t","fields":[{"name":"name","type":"string"}]}""";

    private static byte[] Record(string json)
    {
        var doc = new TqbfDocument(new TqbfFieldRegistry());
        doc.Reset();
        doc.ReadJson(Encoding.UTF8.GetBytes(json));
        return doc.Encode().ToArray();
    }

    [Fact]
    public void Record_MatchesTheRustEncoding()
    {
        // id "7"; two entries in key order: a (id 1) = mixed [int 1, float 2.5], b (id 0) = true.
        byte[] expected =
        [
            0x01, (byte)'7', 0x02,
            0x15, 0x02, 0x0F, 0x00, 0x02, 0x01, 0, 0, 0, 0, 0, 0, 0x04, 0x40,
            0x04,
        ];
        Assert.Equal(expected, Record("""{"id":"7","b":true,"a":[1,2.5]}"""));
    }

    [Fact]
    public void Record_ReadsNumbersAsSerdeJsonDoes()
    {
        // n (id 0) only: tag in the low 4 bits of the entry key.
        static byte Tag(byte[] rec) => (byte)(rec[3] & 0x0F);
        Assert.Equal(1, Tag(Record("""{"id":"1","n":-0}""")));            // -0 is the float -0.0
        Assert.Equal(1, Tag(Record("""{"id":"1","n":1.0}""")));
        Assert.Equal(1, Tag(Record("""{"id":"1","n":1e5}""")));
        Assert.Equal(1, Tag(Record("""{"id":"1","n":-9223372036854775809}""")));
        Assert.Equal(0, Tag(Record("""{"id":"1","n":-9223372036854775808}""")));
        Assert.Equal(8, Tag(Record("""{"id":"1","n":18446744073709551615}""")));
        Assert.Equal(0, Tag(Record("""{"id":"1","n":9223372036854775807}""")));
        var negz = Record("""{"id":"1","n":-0}""");
        Assert.Equal(BitConverter.DoubleToInt64Bits(-0.0), BinaryPrimitives.ReadInt64LittleEndian(negz.AsSpan(4)));
    }

    [Fact]
    public void Record_OrdersKeysByUtf8BytesAndKeepsTheLastDuplicate()
    {
        // Ids by first appearance: é 0, z 1, Z 2; UTF-8 order: Z (0x5A), z (0x7A), é (0xC3 0xA9).
        var rec = Record("""{"id":"1","é":1,"z":2,"Z":3,"z":4}""");
        Assert.Equal(new byte[] { 0x01, (byte)'1', 0x03, 0x20, 0x06, 0x10, 0x08, 0x00, 0x02 }, rec);
    }

    [Fact]
    public void Record_RefusesDocumentsWithoutAStringId()
    {
        Assert.Throws<FormatException>(() => Record("""{"name":"x"}"""));
        Assert.Throws<FormatException>(() => Record("""{"id":1}"""));
        Assert.Throws<FormatException>(() => Record("""[1]"""));
    }

    [Fact]
    public void EventApi_BuildsTheSameRecordAsJson()
    {
        var registry = new TqbfFieldRegistry();
        var doc = new TqbfDocument(registry);
        doc.Reset();
        doc.Property("id").String("7").Property("b").Bool(true).Property("a").StartArray().Int(1).Float(2.5).EndArray();
        Assert.Equal(Record("""{"id":"7","b":true,"a":[1,2.5]}"""), doc.Encode().ToArray());
    }

    [Fact]
    public void File_HasHeaderTableAndCheckedSections()
    {
        string path = Path.Combine(Path.GetTempPath(), $"tqbf-test-{Guid.NewGuid():N}.tqbf");
        try
        {
            using (var w = new TqbfDocsWriter(path, Schema))
            {
                for (int i = 0; i < 3000; i++)
                    w.AddJson($$"""{"id":"d{{i}}","name":"Företag {{i}} {{new string('x', i % 900)}}","n":{{i}}}""");
                w.Complete();
                Assert.Equal(3000u, w.DocumentCount);
            }
            byte[] f = File.ReadAllBytes(path);
            Assert.Equal("TQBF"u8.ToArray(), f[..4]);
            Assert.Equal(6, BinaryPrimitives.ReadUInt16LittleEndian(f.AsSpan(4)));
            Assert.Equal(3000u, BinaryPrimitives.ReadUInt32LittleEndian(f.AsSpan(8)));
            uint n = BinaryPrimitives.ReadUInt32LittleEndian(f.AsSpan(12));
            long tableAt = BinaryPrimitives.ReadInt64LittleEndian(f.AsSpan(32));
            var table = f.AsSpan((int)tableAt, (int)n * 40);
            Assert.Equal(BinaryPrimitives.ReadUInt64LittleEndian(f.AsSpan(40)), XxHash3.HashToUInt64(table));
            var kinds = new List<ushort>();
            for (int i = 0; i < n; i++)
            {
                var e = table.Slice(i * 40, 40);
                long off = BinaryPrimitives.ReadInt64LittleEndian(e[8..]), len = BinaryPrimitives.ReadInt64LittleEndian(e[16..]);
                Assert.Equal(BinaryPrimitives.ReadUInt64LittleEndian(e[32..]), XxHash3.HashToUInt64(f.AsSpan((int)off, (int)len)));
                kinds.Add(BinaryPrimitives.ReadUInt16LittleEndian(e));
                if (kinds[^1] == 5)
                {
                    var rec = f.AsSpan((int)off, (int)len);
                    Assert.Equal(3000u, BinaryPrimitives.ReadUInt32LittleEndian(rec[^8..]));
                    uint chunks = BinaryPrimitives.ReadUInt32LittleEndian(rec[^4..]);
                    Assert.True(chunks > 1);
                    // First chunk: n_docs, raw_len, zlib stream of varint-prefixed records.
                    uint raw = BinaryPrimitives.ReadUInt32LittleEndian(rec[4..]);
                    long next = BinaryPrimitives.ReadInt64LittleEndian(rec[(rec.Length - 8 - 8 * ((int)chunks + 1) + 8)..]);
                    using var z = new ZLibStream(new MemoryStream(rec[8..(int)next].ToArray()), CompressionMode.Decompress);
                    var ms = new MemoryStream();
                    z.CopyTo(ms);
                    Assert.Equal(raw, (uint)ms.Length);
                }
            }
            Assert.Equal(new ushort[] { 4, 5, 1 }, kinds);
        }
        finally
        {
            File.Delete(path);
        }
    }
}
