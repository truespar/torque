using Torque.Http.IntegrationTests.Fixtures;
using Torque.Http.Models;

namespace Torque.Http.IntegrationTests;

[Collection("Torque")]
public class KeyIntegrationTests(TorqueFixture fixture)
{
    private TorqueHttpClient CreateClient() => new(new TorqueHttpClientOptions
    {
        BaseUrl = fixture.HttpUrl,
        ApiKey = fixture.ApiKey,
    });

    [Fact]
    public async Task Key_CreateGetListDelete()
    {
        using var client = CreateClient();
        ulong keyId = 0;

        try
        {
            var created = await client.Keys.CreateAsync(new CreateKeyRequest
            {
                Description = "Integration test key",
                Actions = ["search"],
                Collections = ["*"],
            });

            keyId = created.Id;
            Assert.NotNull(created.Value);
            Assert.True(created.Id > 0);

            var fetched = await client.Keys.GetAsync(created.Id);
            Assert.Equal(created.Id, fetched.Id);
            Assert.Equal("Integration test key", fetched.Description);

            var all = await client.Keys.ListAsync();
            Assert.Contains(all, k => k.Id == created.Id);

            await client.Keys.DeleteAsync(created.Id);
            keyId = 0;
        }
        finally
        {
            if (keyId > 0)
                try { await client.Keys.DeleteAsync(keyId); } catch { }
        }
    }
}
