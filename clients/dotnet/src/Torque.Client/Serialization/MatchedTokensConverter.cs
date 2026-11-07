using System.Text.Json;
using System.Text.Json.Serialization;

namespace Torque.Http.Serialization;

/// <summary>
/// A highlight's matched_tokens: a list of tokens for a string field, a list per element for a string[] field
/// (Typesense's format), read as one list.
/// </summary>
public sealed class MatchedTokensConverter : JsonConverter<List<string>>
{
    public override List<string> Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
    {
        if (reader.TokenType != JsonTokenType.StartArray)
            throw new JsonException("Expected StartArray for matched_tokens");

        var tokens = new List<string>();
        int depth = 0;
        while (reader.Read())
        {
            switch (reader.TokenType)
            {
                case JsonTokenType.StartArray:
                    depth++;
                    break;
                case JsonTokenType.EndArray when depth == 0:
                    return tokens;
                case JsonTokenType.EndArray:
                    depth--;
                    break;
                case JsonTokenType.String:
                    tokens.Add(reader.GetString()!);
                    break;
                default:
                    reader.Skip();
                    break;
            }
        }
        throw new JsonException("Unterminated matched_tokens");
    }

    public override void Write(Utf8JsonWriter writer, List<string> value, JsonSerializerOptions options)
    {
        writer.WriteStartArray();
        foreach (var token in value)
            writer.WriteStringValue(token);
        writer.WriteEndArray();
    }
}
