using System.Text.Json.Serialization;

namespace Torque.Http.Models;

/// <summary>
/// Result of deleting a document.
/// </summary>
public sealed class DeleteResult
{
    /// <summary>ID of the deleted document.</summary>
    [JsonPropertyName("id")]
    public string Id { get; set; } = "";
}
