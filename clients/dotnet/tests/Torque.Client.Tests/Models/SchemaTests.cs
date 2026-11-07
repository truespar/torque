using System.Text.Json;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Tests.Models;

public class SchemaTests
{
    [Fact]
    public void CollectionSchema_JsonRoundtrip()
    {
        var schema = new CollectionSchema
        {
            Name = "products",
            Fields =
            [
                new() { Name = "title", Type = TorqueFieldType.String, Index = true, Stem = true },
                new() { Name = "price", Type = TorqueFieldType.Int32, Sort = true, Optional = true },
                new() { Name = "embedding", Type = TorqueFieldType.FloatArray, NumDim = 384, VecDist = VecDist.Cosine },
            ],
            DefaultSortingField = "price",
        };

        var json = JsonSerializer.Serialize(schema, TorqueJsonContext.Default.CollectionSchema);
        var deserialized = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.CollectionSchema)!;

        Assert.Equal("products", deserialized.Name);
        Assert.Equal(3, deserialized.Fields.Count);
        Assert.Equal("title", deserialized.Fields[0].Name);
        Assert.Equal(TorqueFieldType.String, deserialized.Fields[0].Type);
        Assert.Equal(384, deserialized.Fields[2].NumDim);
        Assert.Equal(VecDist.Cosine, deserialized.Fields[2].VecDist);
        Assert.Equal("price", deserialized.DefaultSortingField);
    }

    [Fact]
    public void FieldType_MatchesRustAsStr()
    {
        Assert.Equal("string", TorqueFieldType.String);
        Assert.Equal("int32", TorqueFieldType.Int32);
        Assert.Equal("int64", TorqueFieldType.Int64);
        Assert.Equal("float", TorqueFieldType.Float);
        Assert.Equal("bool", TorqueFieldType.Bool);
        Assert.Equal("string[]", TorqueFieldType.StringArray);
        Assert.Equal("int32[]", TorqueFieldType.Int32Array);
        Assert.Equal("int64[]", TorqueFieldType.Int64Array);
        Assert.Equal("float[]", TorqueFieldType.FloatArray);
        Assert.Equal("geopoint", TorqueFieldType.GeoPoint);
    }

    [Fact]
    public void VecDist_JsonValues()
    {
        // Verify JSON serialization matches Rust serde renames
        var cosineJson = JsonSerializer.Serialize(VecDist.Cosine);
        var ipJson = JsonSerializer.Serialize(VecDist.DotProduct);
        var l2Json = JsonSerializer.Serialize(VecDist.Euclidean);

        Assert.Equal("\"cosine\"", cosineJson);
        Assert.Equal("\"ip\"", ipJson);
        Assert.Equal("\"l2\"", l2Json);
    }

    [Fact]
    public void CollectionResponse_Deserialize()
    {
        var json = """{"name":"test","num_documents":42,"fields":[{"name":"title","type":"string"}],"created_at":1700000000}""";
        var response = JsonSerializer.Deserialize(json, TorqueJsonContext.Default.CollectionResponse)!;

        Assert.Equal("test", response.Name);
        Assert.Equal(42u, response.NumDocuments);
        Assert.Single(response.Fields);
        Assert.Equal(1700000000UL, response.CreatedAt);
    }
}
