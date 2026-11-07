using System.Text.Json.Serialization;

namespace Torque.Http.Models;

internal sealed class KeysListResponse
{
    [JsonPropertyName("keys")]
    public List<ApiKey> Keys { get; set; } = [];
}

internal sealed class PresetsListResponse
{
    [JsonPropertyName("presets")]
    public List<Preset> Presets { get; set; } = [];
}

internal sealed class StopwordsListResponse
{
    [JsonPropertyName("stopwords")]
    public List<StopwordSet> Stopwords { get; set; } = [];
}

internal sealed class SynonymSetsListResponse
{
    [JsonPropertyName("synonym_sets")]
    public List<SynonymSet> SynonymSets { get; set; } = [];
}

internal sealed class AliasesListResponse
{
    [JsonPropertyName("aliases")]
    public List<CollectionAlias> Aliases { get; set; } = [];
}
