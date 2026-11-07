using Torque.Http.Models;

namespace Torque.Http.Tests.Models;

public class FieldValueTests
{
    [Fact]
    public void Null_IsNull()
    {
        var v = FieldValue.Null;
        Assert.True(v.IsNull);
    }

    [Fact]
    public void String_NotNull()
    {
        var v = FieldValue.FromString("hello");
        Assert.False(v.IsNull);
        var sv = Assert.IsType<FieldValue.StringValue>(v);
        Assert.Equal("hello", sv.Value);
    }

    [Fact]
    public void Int32_Value()
    {
        var v = FieldValue.FromInt32(42);
        var iv = Assert.IsType<FieldValue.Int32Value>(v);
        Assert.Equal(42, iv.Value);
    }

    [Fact]
    public void Int64_Value()
    {
        var v = FieldValue.FromInt64(long.MaxValue);
        var lv = Assert.IsType<FieldValue.Int64Value>(v);
        Assert.Equal(long.MaxValue, lv.Value);
    }

    [Fact]
    public void Float64_Value()
    {
        var v = FieldValue.FromFloat64(3.14);
        var fv = Assert.IsType<FieldValue.Float64Value>(v);
        Assert.Equal(3.14, fv.Value, 5);
    }

    [Fact]
    public void Bool_Value()
    {
        Assert.IsType<FieldValue.BoolValue>(FieldValue.FromBool(true));
        Assert.IsType<FieldValue.BoolValue>(FieldValue.FromBool(false));
    }

    [Fact]
    public void GeoPoint_Value()
    {
        var v = FieldValue.FromGeoPoint(59.33, 18.07);
        var gv = Assert.IsType<FieldValue.GeoPointValue>(v);
        Assert.Equal(59.33, gv.Lat, 2);
        Assert.Equal(18.07, gv.Lng, 2);
    }

    [Fact]
    public void StringArray_Value()
    {
        var v = FieldValue.FromStringArray(["a", "b"]);
        var sa = Assert.IsType<FieldValue.StringArrayValue>(v);
        Assert.Equal(["a", "b"], sa.Values);
    }

    [Fact]
    public void Int32Array_Value()
    {
        var v = FieldValue.FromInt32Array([1, 2, 3]);
        var ia = Assert.IsType<FieldValue.Int32ArrayValue>(v);
        Assert.Equal([1, 2, 3], ia.Values);
    }

    [Fact]
    public void Int64Array_Value()
    {
        var v = FieldValue.FromInt64Array([10L, 20L]);
        var la = Assert.IsType<FieldValue.Int64ArrayValue>(v);
        Assert.Equal([10L, 20L], la.Values);
    }

    [Fact]
    public void FloatArray_Value()
    {
        var v = FieldValue.FromFloatArray([0.1f, 0.2f]);
        var fa = Assert.IsType<FieldValue.FloatArrayValue>(v);
        Assert.Equal([0.1f, 0.2f], fa.Values);
    }
}
