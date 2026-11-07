using System.Text.Json;
using System.Text.Json.Serialization;
using Torque.Http.Models;

namespace Torque.Http.Serialization;

/// <summary>
/// Converts FieldValue to/from JSON using typed dispatch.
/// </summary>
public sealed class FieldValueConverter : JsonConverter<FieldValue>
{
    public override FieldValue Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        return reader.TokenType switch
        {
            JsonTokenType.Null => FieldValue.Null,
            JsonTokenType.True => FieldValue.FromBool(true),
            JsonTokenType.False => FieldValue.FromBool(false),
            JsonTokenType.String => FieldValue.FromString(reader.GetString()!),
            JsonTokenType.Number => ReadNumber(ref reader),
            JsonTokenType.StartArray => ReadArray(ref reader),
            JsonTokenType.StartObject => ReadObject(ref reader),
            _ => throw new JsonException($"Unexpected token {reader.TokenType}"),
        };
    }

    private static FieldValue ReadNumber(ref Utf8JsonReader reader)
    {
        if (reader.TryGetInt32(out int i32))
            return FieldValue.FromInt32(i32);
        if (reader.TryGetInt64(out long i64))
            return FieldValue.FromInt64(i64);
        return FieldValue.FromFloat64(reader.GetDouble());
    }

    private static FieldValue ReadArray(ref Utf8JsonReader reader)
    {
        // Read past StartArray
        reader.Read();

        if (reader.TokenType == JsonTokenType.EndArray)
            return FieldValue.FromStringArray([]);

        if (reader.TokenType == JsonTokenType.String || reader.TokenType == JsonTokenType.Null)
        {
            var strings = new List<string?>();
            do
            {
                if (reader.TokenType == JsonTokenType.EndArray) break;
                strings.Add(reader.TokenType == JsonTokenType.Null ? null : reader.GetString()!);
            } while (reader.Read());
            return FieldValue.FromStringArray(strings.ToArray());
        }

        if (reader.TokenType == JsonTokenType.Number)
        {
            var numbers = new List<double>();
            bool allInt32 = true;
            bool allInt64 = true;

            do
            {
                if (reader.TokenType == JsonTokenType.EndArray)
                    break;
                double val = reader.GetDouble();
                numbers.Add(val);
                if (allInt32 && !reader.TryGetInt32(out _))
                    allInt32 = false;
                if (allInt64 && !reader.TryGetInt64(out _))
                    allInt64 = false;
            } while (reader.Read());

            if (allInt32)
                return FieldValue.FromInt32Array(numbers.Select(n => (int?)n).ToArray());
            if (allInt64)
                return FieldValue.FromInt64Array(numbers.Select(n => (long?)n).ToArray());
            return FieldValue.FromFloatArray(numbers.Select(n => (float)n).ToArray());
        }

        throw new JsonException($"Unexpected array element type: {reader.TokenType}");
    }

    private static FieldValue ReadObject(ref Utf8JsonReader reader)
    {
        using var doc = JsonDocument.ParseValue(ref reader);
        var root = doc.RootElement;
        if (root.TryGetProperty("lat", out var lat) && root.TryGetProperty("lng", out var lng))
            return FieldValue.FromGeoPoint(lat.GetDouble(), lng.GetDouble());

        throw new JsonException("Unknown object shape for FieldValue");
    }

    public override void Write(Utf8JsonWriter writer, FieldValue value, JsonSerializerOptions options)
    {
        switch (value)
        {
            case FieldValue.NullValue:
                writer.WriteNullValue();
                break;
            case FieldValue.StringValue s:
                writer.WriteStringValue(s.Value);
                break;
            case FieldValue.Int32Value i:
                writer.WriteNumberValue(i.Value);
                break;
            case FieldValue.Int64Value l:
                writer.WriteNumberValue(l.Value);
                break;
            case FieldValue.Float64Value f:
                writer.WriteNumberValue(f.Value);
                break;
            case FieldValue.BoolValue b:
                writer.WriteBooleanValue(b.Value);
                break;
            case FieldValue.GeoPointValue g:
                writer.WriteStartObject();
                writer.WriteNumber("lat", g.Lat);
                writer.WriteNumber("lng", g.Lng);
                writer.WriteEndObject();
                break;
            case FieldValue.StringArrayValue sa:
                writer.WriteStartArray();
                foreach (var s in sa.Values) { if (s is null) writer.WriteNullValue(); else writer.WriteStringValue(s); }
                writer.WriteEndArray();
                break;
            case FieldValue.Int32ArrayValue ia:
                writer.WriteStartArray();
                foreach (var n in ia.Values) { if (n.HasValue) writer.WriteNumberValue(n.Value); else writer.WriteNullValue(); }
                writer.WriteEndArray();
                break;
            case FieldValue.Int64ArrayValue la:
                writer.WriteStartArray();
                foreach (var n in la.Values) { if (n.HasValue) writer.WriteNumberValue(n.Value); else writer.WriteNullValue(); }
                writer.WriteEndArray();
                break;
            case FieldValue.FloatArrayValue fa:
                writer.WriteStartArray();
                foreach (var n in fa.Values) writer.WriteNumberValue(n);
                writer.WriteEndArray();
                break;
        }
    }
}
