using System.Diagnostics;
using System.Net.Http;

namespace Torque.Http.IntegrationTests.Fixtures;

/// <summary>
/// xUnit fixture that starts a Torque server process for integration testing.
/// Waits for /health to return OK before tests run.
/// </summary>
public sealed class TorqueFixture : IAsyncLifetime
{
    private Process? _process;
    private readonly HttpClient _http = new();

    public string HttpUrl { get; } = "http://localhost:8108";
    public string TcpHost { get; } = "localhost";
    public int TcpPort { get; } = 8119;
    public string ApiKey { get; } = "test-bootstrap-key";

    public async Task InitializeAsync()
    {
        // Check if server is already running
        try
        {
            _http.DefaultRequestHeaders.Add("X-TYPESENSE-API-KEY", ApiKey);
            var response = await _http.GetAsync($"{HttpUrl}/health");
            if (response.IsSuccessStatusCode)
                return; // Server already running, don't start a new one
        }
        catch (HttpRequestException) { }

        // Find the torque project root (navigate up from test output dir)
        var dir = Directory.GetCurrentDirectory();
        while (dir is not null && !File.Exists(Path.Combine(dir, "Cargo.toml")))
            dir = Directory.GetParent(dir)?.FullName;

        if (dir is null)
            throw new InvalidOperationException("Could not find Torque project root (Cargo.toml)");

        _process = new Process
        {
            StartInfo = new ProcessStartInfo
            {
                FileName = "cargo",
                Arguments = "run --release",
                WorkingDirectory = dir,
                UseShellExecute = false,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                Environment =
                {
                    ["TORQUE_API_KEY"] = ApiKey,
                }
            }
        };

        _process.Start();

        // Wait for health endpoint
        var timeout = TimeSpan.FromSeconds(120);
        var sw = Stopwatch.StartNew();
        while (sw.Elapsed < timeout)
        {
            try
            {
                var response = await _http.GetAsync($"{HttpUrl}/health");
                if (response.IsSuccessStatusCode)
                    return;
            }
            catch (HttpRequestException) { }

            await Task.Delay(500);
        }

        throw new TimeoutException("Torque server did not become healthy within timeout");
    }

    public async Task DisposeAsync()
    {
        if (_process is not null && !_process.HasExited)
        {
            _process.Kill(entireProcessTree: true);
            await _process.WaitForExitAsync();
        }
        _process?.Dispose();
        _http.Dispose();
    }
}

[CollectionDefinition("Torque")]
public class TorqueCollection : ICollectionFixture<TorqueFixture> { }
