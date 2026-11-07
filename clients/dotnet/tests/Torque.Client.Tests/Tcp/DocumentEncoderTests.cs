using System.Buffers.Binary;
using System.Text;
using Torque.Http.Models;
using Torque.Http.Tcp;

namespace Torque.Http.Tests.Tcp;

public class DocumentEncoderTests
{
    private static CollectionSchema TestSchema() => new()
    {
        Name = "test",
        Fields =
        [
            new() { Name = "title", Type = TorqueFieldType.String },
            new() { Name = "price", Type = TorqueFieldType.Int32, Optional = true, Sort = true },
            new() { Name = "rating", Type = TorqueFieldType.Float, Optional = true, Sort = true },
            new() { Name = "in_stock", Type = TorqueFieldType.Bool, Optional = true },
            new() { Name = "tags", Type = TorqueFieldType.StringArray, Optional = true, Facet = true },
            new() { Name = "location", Type = TorqueFieldType.GeoPoint, Optional = true },
        ]
    };

    /// <summary>
    /// Helper: encode a document using the new (byte[], ref int) API and return the written slice.
    /// </summary>
    private static byte[] EncodeToBytes(TorqueDocument doc, CollectionSchema schema)
    {
        int size = DocumentEncoder.ComputeSize(doc, schema);
        var buf = new byte[size];
        int pos = 0;
        DocumentEncoder.Encode(doc, schema, buf, ref pos);
        Assert.Equal(size, pos); // ComputeSize must be exact
        return buf;
    }

    /// <summary>
    /// Matches the Rust test document_roundtrip_all_types in protocol.rs.
    /// </summary>
    [Fact]
    public void Encode_AllFieldTypes_MatchesRustFormat()
    {
        var schema = TestSchema();
        var doc = new TorqueDocumentBuilder()
            .SetId("prod-1")
            .SetLanguage(Language.Swedish)
            .Set("title", "Rust Programming")
            .Set("price", 299)
            .Set("rating", 4.5)
            .Set("in_stock", true)
            .Set("tags", new string?[] { "tech", "books" })
            .SetGeoPoint("location", 59.3293, 18.0686)
            .Build(schema);

        var bytes = EncodeToBytes(doc, schema);
        int offset = 0;

        // External ID: u16 len + "prod-1"
        ushort idLen = BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset));
        offset += 2;
        Assert.Equal(6, idLen);
        Assert.Equal("prod-1", Encoding.UTF8.GetString(bytes, offset, idLen));
        offset += idLen;

        // Language: Swedish = 1
        Assert.Equal(1, bytes[offset++]);

        // Field count: 6
        ushort fieldCount = BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset));
        offset += 2;
        Assert.Equal(6, fieldCount);

        // Field 0: title (String) - present
        Assert.Equal(1, bytes[offset++]); // present
        uint titleLen = BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset));
        offset += 4;
        Assert.Equal(16u, titleLen); // "Rust Programming" = 16 bytes
        Assert.Equal("Rust Programming", Encoding.UTF8.GetString(bytes, offset, (int)titleLen));
        offset += (int)titleLen;

        // Field 1: price (Int32) - present
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(299, BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(offset)));
        offset += 4;

        // Field 2: rating (Float64) - present
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(4.5, BinaryPrimitives.ReadDoubleBigEndian(bytes.AsSpan(offset)), 5);
        offset += 8;

        // Field 3: in_stock (Bool) - present, true
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(1, bytes[offset++]);

        // Field 4: tags (StringArray) - present
        Assert.Equal(1, bytes[offset++]);
        ushort tagCount = BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset));
        offset += 2;
        Assert.Equal(2, tagCount);
        // "tech" — null flag + string
        Assert.Equal(1, bytes[offset++]); // present flag
        uint techLen = BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset)); offset += 4;
        Assert.Equal("tech", Encoding.UTF8.GetString(bytes, offset, (int)techLen)); offset += (int)techLen;
        // "books" — null flag + string
        Assert.Equal(1, bytes[offset++]); // present flag
        uint booksLen = BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset)); offset += 4;
        Assert.Equal("books", Encoding.UTF8.GetString(bytes, offset, (int)booksLen)); offset += (int)booksLen;

        // Field 5: location (GeoPoint) - present
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(59.3293, BinaryPrimitives.ReadDoubleBigEndian(bytes.AsSpan(offset)), 4);
        offset += 8;
        Assert.Equal(18.0686, BinaryPrimitives.ReadDoubleBigEndian(bytes.AsSpan(offset)), 4);
        offset += 8;

        // All bytes consumed
        Assert.Equal(bytes.Length, offset);
    }

    [Fact]
    public void Encode_WithNulls_MatchesRustFormat()
    {
        var schema = TestSchema();
        var doc = new TorqueDocumentBuilder()
            .SetId("1")
            .SetLanguage(Language.English)
            .Set("title", "Hello")
            .Build(schema);

        var bytes = EncodeToBytes(doc, schema);
        int offset = 0;

        // External ID: "1"
        ushort idLen = BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset)); offset += 2;
        Assert.Equal(1, idLen);
        offset += idLen;

        // Language: English = 0
        Assert.Equal(0, bytes[offset++]);

        // Field count
        ushort fieldCount = BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset)); offset += 2;
        Assert.Equal(6, fieldCount);

        // title: present
        Assert.Equal(1, bytes[offset++]);
        uint titleLen = BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset)); offset += 4;
        offset += (int)titleLen;

        // remaining 5 fields: null (0 byte each)
        for (int i = 0; i < 5; i++)
        {
            Assert.Equal(0, bytes[offset++]);
        }

        Assert.Equal(bytes.Length, offset);
    }

    [Fact]
    public void EncodeBatch_MultipleDocs()
    {
        var schema = TestSchema();
        var docs = new List<TorqueDocument>
        {
            new TorqueDocumentBuilder().SetId("a").Set("title", "First").Set("price", 10).Build(schema),
            new TorqueDocumentBuilder().SetId("b").SetLanguage(Language.Finnish).Set("title", "Second").Set("price", 20).Build(schema),
        };

        // Encode batch using the new API: compute total size, allocate, encode sequentially
        int totalSize = 0;
        foreach (var doc in docs)
            totalSize += DocumentEncoder.ComputeSize(doc, schema);

        var payload = new byte[totalSize];
        int pos = 0;
        foreach (var doc in docs)
            DocumentEncoder.Encode(doc, schema, payload, ref pos);
        Assert.Equal(totalSize, pos);

        // Should be able to parse both documents sequentially
        int offset = 0;

        // Doc 1: external_id = "a"
        ushort len1 = BinaryPrimitives.ReadUInt16BigEndian(payload.AsSpan(offset)); offset += 2;
        Assert.Equal("a", Encoding.UTF8.GetString(payload, offset, len1)); offset += len1;
        Assert.Equal(0, payload[offset++]); // English

        // Skip rest of doc 1 fields
        ushort fc1 = BinaryPrimitives.ReadUInt16BigEndian(payload.AsSpan(offset)); offset += 2;
        Assert.Equal(6, fc1);

        // title field - present + "First"
        Assert.Equal(1, payload[offset++]);
        uint fl1 = BinaryPrimitives.ReadUInt32BigEndian(payload.AsSpan(offset)); offset += 4;
        offset += (int)fl1;
        // price - present + i32
        Assert.Equal(1, payload[offset++]);
        offset += 4;
        // remaining 4 nulls
        for (int i = 0; i < 4; i++) Assert.Equal(0, payload[offset++]);

        // Doc 2: external_id = "b"
        ushort len2 = BinaryPrimitives.ReadUInt16BigEndian(payload.AsSpan(offset)); offset += 2;
        Assert.Equal("b", Encoding.UTF8.GetString(payload, offset, len2)); offset += len2;
        Assert.Equal(4, payload[offset++]); // Finnish

        Assert.True(payload.Length > offset); // more data follows
    }

    [Fact]
    public void Encode_Int32Array_And_Int64Array()
    {
        var schema = new CollectionSchema
        {
            Name = "test",
            Fields =
            [
                new() { Name = "ids32", Type = TorqueFieldType.Int32Array },
                new() { Name = "ids64", Type = TorqueFieldType.Int64Array },
            ]
        };

        var doc = new TorqueDocumentBuilder()
            .SetId("x")
            .Set("ids32", new int?[] { 1, 2, 3 })
            .Set("ids64", new long?[] { 100L, 200L })
            .Build(schema);

        var bytes = EncodeToBytes(doc, schema);
        int offset = 0;

        // Skip external_id, language, field_count
        ushort idLen = BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset)); offset += 2 + idLen;
        offset += 1; // language
        offset += 2; // field count

        // ids32: present, count=3, values 1,2,3 (each with null flag)
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(3, BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset))); offset += 2;
        Assert.Equal(1, bytes[offset++]); // present flag
        Assert.Equal(1, BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(offset))); offset += 4;
        Assert.Equal(1, bytes[offset++]); // present flag
        Assert.Equal(2, BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(offset))); offset += 4;
        Assert.Equal(1, bytes[offset++]); // present flag
        Assert.Equal(3, BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(offset))); offset += 4;

        // ids64: present, count=2, values 100,200 (each with null flag)
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(2, BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset))); offset += 2;
        Assert.Equal(1, bytes[offset++]); // present flag
        Assert.Equal(100L, BinaryPrimitives.ReadInt64BigEndian(bytes.AsSpan(offset))); offset += 8;
        Assert.Equal(1, bytes[offset++]); // present flag
        Assert.Equal(200L, BinaryPrimitives.ReadInt64BigEndian(bytes.AsSpan(offset))); offset += 8;

        Assert.Equal(bytes.Length, offset);
    }

    [Fact]
    public void Encode_FloatArray()
    {
        var schema = new CollectionSchema
        {
            Name = "test",
            Fields = [new() { Name = "embedding", Type = TorqueFieldType.FloatArray, NumDim = 3 }]
        };

        var doc = new TorqueDocumentBuilder()
            .SetId("v1")
            .Set("embedding", new float[] { 0.1f, 0.2f, 0.3f })
            .Build(schema);

        var bytes = EncodeToBytes(doc, schema);
        int offset = 0;

        // Skip header
        ushort idLen = BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset)); offset += 2 + idLen;
        offset += 1; // language
        offset += 2; // field count

        // embedding: present, count=3, f32 values (no null flags for float[])
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(3, BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset))); offset += 2;
        Assert.Equal(0.1f, BinaryPrimitives.ReadSingleBigEndian(bytes.AsSpan(offset)), 5); offset += 4;
        Assert.Equal(0.2f, BinaryPrimitives.ReadSingleBigEndian(bytes.AsSpan(offset)), 5); offset += 4;
        Assert.Equal(0.3f, BinaryPrimitives.ReadSingleBigEndian(bytes.AsSpan(offset)), 5); offset += 4;

        Assert.Equal(bytes.Length, offset);
    }

    [Fact]
    public void ComputeSize_ExactMatch()
    {
        var schema = TestSchema();
        var doc = new TorqueDocumentBuilder()
            .SetId("prod-1")
            .SetLanguage(Language.Swedish)
            .Set("title", "Rust Programming")
            .Set("price", 299)
            .Set("rating", 4.5)
            .Set("in_stock", true)
            .Set("tags", new string?[] { "tech", "books" })
            .SetGeoPoint("location", 59.3293, 18.0686)
            .Build(schema);

        int computed = DocumentEncoder.ComputeSize(doc, schema);
        var buf = new byte[computed];
        int pos = 0;
        DocumentEncoder.Encode(doc, schema, buf, ref pos);

        Assert.Equal(computed, pos);
    }

    [Fact]
    public void Encode_NullableArrayElements()
    {
        var schema = new CollectionSchema
        {
            Name = "test",
            Fields =
            [
                new() { Name = "names", Type = TorqueFieldType.StringArray },
                new() { Name = "scores", Type = TorqueFieldType.Int32Array },
                new() { Name = "big_ids", Type = TorqueFieldType.Int64Array },
            ]
        };

        var doc = new TorqueDocumentBuilder()
            .SetId("n1")
            .Set("names", new string?[] { "hello", null, "world" })
            .Set("scores", new int?[] { 10, null, 30 })
            .Set("big_ids", new long?[] { null, 200L })
            .Build(schema);

        var bytes = EncodeToBytes(doc, schema);
        int offset = 0;

        // Skip header
        ushort idLen = BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset)); offset += 2 + idLen;
        offset += 1; // language
        offset += 2; // field count

        // names: present, count=3
        Assert.Equal(1, bytes[offset++]); // field present
        Assert.Equal(3, BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset))); offset += 2;
        // "hello" — present
        Assert.Equal(1, bytes[offset++]);
        uint helloLen = BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset)); offset += 4;
        Assert.Equal("hello", Encoding.UTF8.GetString(bytes, offset, (int)helloLen)); offset += (int)helloLen;
        // null
        Assert.Equal(0, bytes[offset++]);
        // "world" — present
        Assert.Equal(1, bytes[offset++]);
        uint worldLen = BinaryPrimitives.ReadUInt32BigEndian(bytes.AsSpan(offset)); offset += 4;
        Assert.Equal("world", Encoding.UTF8.GetString(bytes, offset, (int)worldLen)); offset += (int)worldLen;

        // scores: present, count=3
        Assert.Equal(1, bytes[offset++]); // field present
        Assert.Equal(3, BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset))); offset += 2;
        // 10 — present
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(10, BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(offset))); offset += 4;
        // null
        Assert.Equal(0, bytes[offset++]);
        // 30 — present
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(30, BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(offset))); offset += 4;

        // big_ids: present, count=2
        Assert.Equal(1, bytes[offset++]); // field present
        Assert.Equal(2, BinaryPrimitives.ReadUInt16BigEndian(bytes.AsSpan(offset))); offset += 2;
        // null
        Assert.Equal(0, bytes[offset++]);
        // 200 — present
        Assert.Equal(1, bytes[offset++]);
        Assert.Equal(200L, BinaryPrimitives.ReadInt64BigEndian(bytes.AsSpan(offset))); offset += 8;

        Assert.Equal(bytes.Length, offset);
    }
}
