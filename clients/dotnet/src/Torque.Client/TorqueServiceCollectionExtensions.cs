namespace Torque.Http;

/// <summary>
/// Factory for creating <see cref="ITorqueHttpClient"/> instances, suitable for DI registration.
/// <para>
/// Usage with <c>Microsoft.Extensions.DependencyInjection</c>:
/// <code>
/// services.AddSingleton&lt;ITorqueHttpClient&gt;(sp =&gt;
///     TorqueHttpClientFactory.Create(options =&gt;
///     {
///         options.ApiKey = "your-api-key";
///         options.BaseUrl = "http://localhost:8108";
///     }));
/// </code>
/// </para>
/// <para>
/// Usage with <c>IHttpClientFactory</c>:
/// <code>
/// services.AddHttpClient("Torque");
/// services.AddSingleton&lt;ITorqueHttpClient&gt;(sp =&gt;
/// {
///     var factory = sp.GetRequiredService&lt;IHttpClientFactory&gt;();
///     var httpClient = factory.CreateClient("Torque");
///     return TorqueHttpClientFactory.Create(httpClient, options =&gt;
///     {
///         options.ApiKey = "your-api-key";
///     });
/// });
/// </code>
/// </para>
/// </summary>
public static class TorqueHttpClientFactory
{
    /// <summary>
    /// Creates a new <see cref="ITorqueHttpClient"/> with the specified options.
    /// The client owns and disposes the underlying <see cref="HttpClient"/>.
    /// </summary>
    /// <param name="configure">Action to configure <see cref="TorqueHttpClientOptions"/>.</param>
    /// <returns>A configured <see cref="ITorqueHttpClient"/> instance.</returns>
    public static ITorqueHttpClient Create(Action<TorqueHttpClientOptions> configure)
    {
        var options = new TorqueHttpClientOptions { ApiKey = "" };
        configure(options);
        ValidateOptions(options);
        return new TorqueHttpClient(options);
    }

    /// <summary>
    /// Creates a new <see cref="ITorqueHttpClient"/> using a caller-provided <see cref="HttpClient"/>.
    /// Ideal for <c>IHttpClientFactory</c> integration — the caller owns the HttpClient lifecycle.
    /// </summary>
    /// <param name="httpClient">The HttpClient to use for HTTP requests.</param>
    /// <param name="configure">Action to configure <see cref="TorqueHttpClientOptions"/>.</param>
    /// <returns>A configured <see cref="ITorqueHttpClient"/> instance.</returns>
    public static ITorqueHttpClient Create(HttpClient httpClient, Action<TorqueHttpClientOptions> configure)
    {
        var options = new TorqueHttpClientOptions { ApiKey = "" };
        configure(options);
        ValidateOptions(options);
        return new TorqueHttpClient(options, httpClient);
    }

    private static void ValidateOptions(TorqueHttpClientOptions options)
    {
        if (string.IsNullOrEmpty(options.ApiKey))
            throw new ArgumentException("TorqueHttpClientOptions.ApiKey must be set.");
        if (options.Nodes.Count == 0)
            throw new ArgumentException("TorqueHttpClientOptions.Nodes must contain at least one node.");
    }
}

// Backward compatibility
/// <summary>Backward compatibility alias for <see cref="TorqueHttpClientFactory"/>.</summary>
public static class TorqueClientFactory
{
    /// <summary>Creates a new client with the specified options.</summary>
    public static ITorqueHttpClient Create(Action<TorqueHttpClientOptions> configure) =>
        TorqueHttpClientFactory.Create(configure);

    /// <summary>Creates a new client using a caller-provided HttpClient.</summary>
    public static ITorqueHttpClient Create(HttpClient httpClient, Action<TorqueHttpClientOptions> configure) =>
        TorqueHttpClientFactory.Create(httpClient, configure);
}
