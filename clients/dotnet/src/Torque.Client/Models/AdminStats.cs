namespace Torque.Http.Models;

/// <summary>
/// Server admin statistics including memory and per-collection breakdowns.
/// </summary>
public sealed class AdminStats
{
    /// <summary>System-level statistics.</summary>
    public SystemStats System { get; set; } = new();

    /// <summary>Per-collection statistics.</summary>
    public List<CollectionStats> Collections { get; set; } = [];
}

/// <summary>
/// System-level statistics.
/// </summary>
public sealed class SystemStats
{
    /// <summary>Process memory statistics.</summary>
    public MemoryStats Memory { get; set; } = new();
}

/// <summary>
/// Process memory statistics from the allocator.
/// </summary>
public sealed class MemoryStats
{
    /// <summary>Total bytes allocated by the allocator.</summary>
    public long AllocatedBytes { get; set; }

    /// <summary>Resident set size in bytes.</summary>
    public long ResidentBytes { get; set; }
}

/// <summary>
/// Statistics for a single collection.
/// </summary>
public sealed class CollectionStats
{
    /// <summary>Collection name.</summary>
    public string Name { get; set; } = "";

    /// <summary>Number of documents in the collection.</summary>
    public int NumDocuments { get; set; }

    /// <summary>Memory breakdown by index component, if available.</summary>
    public IndexMemoryBreakdown? Memory { get; set; }
}

/// <summary>
/// Per-component memory breakdown for a collection index.
/// </summary>
public sealed class IndexMemoryBreakdown
{
    /// <summary>Total index memory in bytes.</summary>
    public long TotalBytes { get; set; }

    /// <summary>Memory used by document storage.</summary>
    public long DocsBytes { get; set; }

    /// <summary>Memory used by the inverted index.</summary>
    public long InvertedBytes { get; set; }

    /// <summary>Memory used by string filter indices.</summary>
    public long StringFilterBytes { get; set; }

    /// <summary>Memory used by numeric filter indices.</summary>
    public long NumericFilterBytes { get; set; }

    /// <summary>Memory used by boolean filter indices.</summary>
    public long BoolFilterBytes { get; set; }

    /// <summary>Memory used by sort indices.</summary>
    public long SortBytes { get; set; }

    /// <summary>Memory used by geo indices.</summary>
    public long GeoBytes { get; set; }

    /// <summary>Memory used by vector indices.</summary>
    public long VectorBytes { get; set; }

    /// <summary>Memory used by the all-documents bitmap.</summary>
    public long AllDocsBitmapBytes { get; set; }

    /// <summary>Memory used by SymSpell typo correction data.</summary>
    public long SymspellBytes { get; set; }

    /// <summary>Memory used by prefix search data.</summary>
    public long PrefixBytes { get; set; }

    /// <summary>Memory used by the document ID map.</summary>
    public long IdMapBytes { get; set; }
}
