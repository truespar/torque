namespace Torque.Http.Models;

/// <summary>
/// Discriminated union for field values matching Rust FieldValue.
/// Use the static factory methods to create instances.
/// </summary>
public abstract class FieldValue
{
    private FieldValue() { }

    /// <summary>Singleton null field value representing an absent optional field.</summary>
    public static FieldValue Null => NullValue.Instance;

    /// <summary>Creates a string field value.</summary>
    /// <param name="value">The string value.</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the string.</returns>
    public static FieldValue FromString(string value) => new StringValue(value);

    /// <summary>Creates an int32 field value.</summary>
    /// <param name="value">The int32 value.</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the int32.</returns>
    public static FieldValue FromInt32(int value) => new Int32Value(value);

    /// <summary>Creates an int64 field value.</summary>
    /// <param name="value">The int64 value.</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the int64.</returns>
    public static FieldValue FromInt64(long value) => new Int64Value(value);

    /// <summary>Creates a float64 field value.</summary>
    /// <param name="value">The float64 value.</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the float64.</returns>
    public static FieldValue FromFloat64(double value) => new Float64Value(value);

    /// <summary>Creates a boolean field value.</summary>
    /// <param name="value">The boolean value.</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the boolean.</returns>
    public static FieldValue FromBool(bool value) => new BoolValue(value);

    /// <summary>Creates a geopoint field value from latitude and longitude.</summary>
    /// <param name="lat">Latitude in degrees.</param>
    /// <param name="lng">Longitude in degrees.</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the geopoint.</returns>
    public static FieldValue FromGeoPoint(double lat, double lng) => new GeoPointValue(lat, lng);

    /// <summary>Creates a string array field value.</summary>
    /// <param name="values">The string array (null elements represent missing values).</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the string array.</returns>
    public static FieldValue FromStringArray(string?[] values) => new StringArrayValue(values);

    /// <summary>Creates a nullable int32 array field value.</summary>
    /// <param name="values">The nullable int32 array.</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the int32 array.</returns>
    public static FieldValue FromInt32Array(int?[] values) => new Int32ArrayValue(values);

    /// <summary>Creates a nullable int64 array field value.</summary>
    /// <param name="values">The nullable int64 array.</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the int64 array.</returns>
    public static FieldValue FromInt64Array(long?[] values) => new Int64ArrayValue(values);

    /// <summary>Creates a float array field value (used for vector embeddings).</summary>
    /// <param name="values">The float array.</param>
    /// <returns>A new <see cref="FieldValue"/> wrapping the float array.</returns>
    public static FieldValue FromFloatArray(float[] values) => new FloatArrayValue(values);

    /// <summary>Returns true if this value represents a null/absent field.</summary>
    public bool IsNull => this is NullValue;

    internal sealed class NullValue : FieldValue
    {
        public static readonly NullValue Instance = new();
    }

    internal sealed class StringValue : FieldValue
    {
        public string Value { get; }
        public int Utf8ByteLength { get; }

        public StringValue(string value)
        {
            Value = value;
            Utf8ByteLength = System.Text.Encoding.UTF8.GetByteCount(value);
        }
    }

    internal sealed class Int32Value(int value) : FieldValue
    {
        public int Value { get; } = value;
    }

    internal sealed class Int64Value(long value) : FieldValue
    {
        public long Value { get; } = value;
    }

    internal sealed class Float64Value(double value) : FieldValue
    {
        public double Value { get; } = value;
    }

    internal sealed class BoolValue(bool value) : FieldValue
    {
        public bool Value { get; } = value;
    }

    internal sealed class GeoPointValue(double lat, double lng) : FieldValue
    {
        public double Lat { get; } = lat;
        public double Lng { get; } = lng;
    }

    internal sealed class StringArrayValue : FieldValue
    {
        public string?[] Values { get; }
        public int[] Utf8ByteLengths { get; }
        public int TotalUtf8Bytes { get; }

        public StringArrayValue(string?[] values)
        {
            Values = values;
            Utf8ByteLengths = new int[values.Length];
            int total = 0;
            for (int i = 0; i < values.Length; i++)
            {
                if (values[i] is null) continue;
                int len = System.Text.Encoding.UTF8.GetByteCount(values[i]!);
                Utf8ByteLengths[i] = len;
                total += len;
            }
            TotalUtf8Bytes = total;
        }
    }

    internal sealed class Int32ArrayValue(int?[] values) : FieldValue
    {
        public int?[] Values { get; } = values;
    }

    internal sealed class Int64ArrayValue(long?[] values) : FieldValue
    {
        public long?[] Values { get; } = values;
    }

    internal sealed class FloatArrayValue(float[] values) : FieldValue
    {
        public float[] Values { get; } = values;
    }
}
