using System.Text.Json;
using System.Text.Json.Serialization;
using Torque.Http.Models;

namespace Torque.Http.Serialization;

[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.SnakeCaseLower,
    DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    WriteIndented = false)]
[JsonSerializable(typeof(CollectionSchema))]
[JsonSerializable(typeof(CollectionResponse))]
[JsonSerializable(typeof(List<CollectionResponse>))]
[JsonSerializable(typeof(FieldDefinition))]
[JsonSerializable(typeof(SearchResult))]
[JsonSerializable(typeof(GroupedSearchResult))]
[JsonSerializable(typeof(MultiSearchRequest))]
[JsonSerializable(typeof(MultiSearchResponse))]
[JsonSerializable(typeof(ApiKey))]
[JsonSerializable(typeof(KeysListResponse))]
[JsonSerializable(typeof(CreateKeyRequest))]
[JsonSerializable(typeof(CollectionAlias))]
[JsonSerializable(typeof(AliasesListResponse))]
[JsonSerializable(typeof(UpsertAliasRequest))]
[JsonSerializable(typeof(SynonymSet))]
[JsonSerializable(typeof(SynonymSetsListResponse))]
[JsonSerializable(typeof(SynonymItem))]
[JsonSerializable(typeof(List<SynonymItem>))]
[JsonSerializable(typeof(Preset))]
[JsonSerializable(typeof(PresetsListResponse))]
[JsonSerializable(typeof(StopwordSet))]
[JsonSerializable(typeof(StopwordsListResponse))]
[JsonSerializable(typeof(HealthResponse))]
[JsonSerializable(typeof(ReadyResponse))]
[JsonSerializable(typeof(ImportResult))]
[JsonSerializable(typeof(ErrorResponse))]
[JsonSerializable(typeof(AdminStats))]
[JsonSerializable(typeof(JsonElement))]
[JsonSerializable(typeof(DeleteResult))]
[JsonSerializable(typeof(Dictionary<string, object?>))]
internal partial class TorqueJsonContext : JsonSerializerContext
{
}

internal sealed class ErrorResponse
{
    [JsonPropertyName("message")]
    public string Message { get; set; } = "";
}
