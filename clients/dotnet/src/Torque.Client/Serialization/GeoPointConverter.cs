using System.Text.Json;
using System.Text.Json.Serialization;

namespace Torque.Http.Serialization;

/// <summary>
/// GeoPoint as {"lat": x, "lng": y}.
/// </summary>
public readonly record struct GeoPoint(double Lat, double Lng);

public sealed class GeoPointConverter : JsonConverter<GeoPoint>
{
    public override GeoPoint Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        if (reader.TokenType != JsonTokenType.StartObject)
            throw new JsonException("Expected StartObject for GeoPoint");

        double lat = 0, lng = 0;
        while (reader.Read() && reader.TokenType != JsonTokenType.EndObject)
        {
            if (reader.TokenType == JsonTokenType.PropertyName)
            {
                var prop = reader.GetString();
                reader.Read();
                if (prop == "lat") lat = reader.GetDouble();
                else if (prop == "lng") lng = reader.GetDouble();
            }
        }
        return new GeoPoint(lat, lng);
    }

    public override void Write(Utf8JsonWriter writer, GeoPoint value, JsonSerializerOptions options)
    {
        writer.WriteStartObject();
        writer.WriteNumber("lat", value.Lat);
        writer.WriteNumber("lng", value.Lng);
        writer.WriteEndObject();
    }
}
