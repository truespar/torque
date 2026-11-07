using System.Text.Json;
using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class PresetIntegrationTests(TorqueFixture fixture)
{
    private TorqueHttpClient CreateClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = fixture.HttpUrl,
        ApiKey = fixture.ApiKey,
    });

    [Fact]
    public async Task Preset_CrudCycle()
    {
        using var client = CreateClient();
        var presetId = $"preset_{Guid.NewGuid():N}"[..32];

        try
        {
            var preset = new Preset
            {
                Name = presetId,
                Value = JsonSerializer.SerializeToElement(new { sort_by = "rating:desc", per_page = 20 }),
            };

            await client.Presets.UpsertAsync(presetId, preset);
            var fetched = await client.Presets.GetAsync(presetId);
            Assert.Equal(presetId, fetched.Name);

            var all = await client.Presets.ListAsync();
            Assert.Contains(all, p => p.Name == presetId);

            await client.Presets.DeleteAsync(presetId);
        }
        finally
        {
            try { await client.Presets.DeleteAsync(presetId); } catch { }
        }
    }
}
