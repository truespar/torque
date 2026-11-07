namespace Torque.Http.Models;

/// <summary>
/// Options for exporting documents from a collection.
/// </summary>
public sealed class ExportOptions
{
    /// <summary>Filter expression to limit exported documents.</summary>
    public string? FilterBy { get; set; }

    /// <summary>Comma-separated list of fields to include in exported documents.</summary>
    public string? IncludeFields { get; set; }

    /// <summary>Comma-separated list of fields to exclude from exported documents.</summary>
    public string? ExcludeFields { get; set; }
}
