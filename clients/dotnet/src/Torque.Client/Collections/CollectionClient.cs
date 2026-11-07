using Torque.Http.Http;
using Torque.Http.Models;
using Torque.Http.Serialization;

namespace Torque.Http.Collections;

/// <summary>
/// Collection CRUD operations.
/// </summary>
public sealed class CollectionClient
{
    private readonly HttpTransport _transport;

    internal CollectionClient(HttpTransport transport) => _transport = transport;

    /// <summary>
    /// Creates a new collection with the specified schema.
    /// </summary>
    /// <param name="schema">The collection schema defining fields, name, and settings.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The created collection with server-assigned metadata.</returns>
    public Task<CollectionResponse> CreateAsync(CollectionSchema schema, CancellationToken ct = default) =>
        _transport.PostAsync("/collections", schema,
            TorqueJsonContext.Default.CollectionSchema,
            TorqueJsonContext.Default.CollectionResponse, ct);

    /// <summary>
    /// Lists all collections on the server.
    /// </summary>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>A list of all collections.</returns>
    public Task<List<CollectionResponse>> ListAsync(CancellationToken ct = default) =>
        _transport.GetAsync("/collections", TorqueJsonContext.Default.ListCollectionResponse, ct);

    /// <summary>
    /// Retrieves a single collection by name.
    /// </summary>
    /// <param name="name">The collection name.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The collection details including field definitions and document count.</returns>
    public Task<CollectionResponse> GetAsync(string name, CancellationToken ct = default) =>
        _transport.GetAsync($"/collections/{Uri.EscapeDataString(name)}",
            TorqueJsonContext.Default.CollectionResponse, ct);

    /// <summary>
    /// Updates a collection schema by adding or dropping fields.
    /// </summary>
    /// <param name="name">The collection name.</param>
    /// <param name="schema">The schema update containing fields to add or drop.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The updated collection.</returns>
    public Task<CollectionResponse> UpdateAsync(string name, CollectionSchema schema, CancellationToken ct = default) =>
        _transport.PatchAsync($"/collections/{Uri.EscapeDataString(name)}", schema,
            TorqueJsonContext.Default.CollectionSchema,
            TorqueJsonContext.Default.CollectionResponse, ct);

    /// <summary>
    /// Creates collection <paramref name="name"/> from segment files: the <c>seg_*.tqbf</c> files of
    /// <c>torque-admin build</c>, or a docs-only file written by <see cref="Tqbf.TqbfDocsWriter"/>
    /// (the server then builds it). Each file is one request, streamed from disk; the request that
    /// completes the set creates the collection. Point an alias at it with the alias client to
    /// switch readers. Large files need a long <see cref="TorqueHttpClientOptions.Timeout"/>.
    /// </summary>
    /// <param name="name">The new collection's name.</param>
    /// <param name="files">The segment files, in segment order.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The created collection.</returns>
    public async Task<CollectionResponse> UploadSegmentsAsync(string name, IReadOnlyList<string> files, CancellationToken ct = default)
    {
        if (files.Count == 0) throw new ArgumentException("No segment files.", nameof(files));
        string body = "";
        for (int i = 0; i < files.Count; i++)
        {
            body = await _transport.PutFileAsync(
                $"/collections/{Uri.EscapeDataString(name)}/segments?part={i}&parts={files.Count}",
                files[i], ct).ConfigureAwait(false);
        }
        return System.Text.Json.JsonSerializer.Deserialize(body, TorqueJsonContext.Default.CollectionResponse)!;
    }

    /// <summary>
    /// Permanently deletes a collection and all its documents.
    /// </summary>
    /// <param name="name">The collection name.</param>
    /// <param name="ct">Cancellation token.</param>
    public Task DeleteAsync(string name, CancellationToken ct = default) =>
        _transport.DeleteAsync($"/collections/{Uri.EscapeDataString(name)}", ct);

    /// <summary>
    /// Truncates a collection, removing all documents but keeping the schema.
    /// </summary>
    /// <param name="name">The collection name.</param>
    /// <param name="ct">Cancellation token.</param>
    /// <returns>The delete result with the number of documents removed.</returns>
    public Task<DeleteResult> TruncateAsync(string name, CancellationToken ct = default) =>
        _transport.DeleteAsync(
            $"/collections/{Uri.EscapeDataString(name)}/documents?truncate=true",
            TorqueJsonContext.Default.DeleteResult, ct);
}
