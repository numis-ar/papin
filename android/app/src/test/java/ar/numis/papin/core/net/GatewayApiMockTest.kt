package ar.numis.papin.core.net

import kotlinx.coroutines.test.runTest
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class GatewayApiMockTest {
    private fun newApi(server: MockWebServer, token: String = "tok") =
        GatewayApi(server.url("/").toString().removeSuffix("/"), token, okhttp3.OkHttpClient())

    @Test
    fun listAgentsParsesRecords() = runTest {
        val server = MockWebServer()
        server.enqueue(
            MockResponse().setBody(
                """[{"id":"a1","name":"One","created_at":123,"state":"active","config":{"base":"fake"}}]""",
            ).addHeader("Content-Type", "application/json"),
        )
        server.start()
        val agents = newApi(server).listAgents()
        assertEquals(1, agents.size)
        assertEquals("a1", agents[0].id)
        assertEquals("active", agents[0].state)
        assertEquals("fake", agents[0].config?.base)
        val request = server.takeRequest()
        assertEquals("Bearer tok", request.getHeader("Authorization"))
        server.shutdown()
    }

    @Test
    fun configCatalogParsesBasesSeedsEnv() = runTest {
        val server = MockWebServer()
        server.enqueue(
            MockResponse().setBody(
                """{"bases":[{"name":"node22","description":"d"}],"seeds":[{"name":"demo"}],"env":[{"key":"K","default":"1"}]}""",
            ),
        )
        server.start()
        val catalog = newApi(server).configCatalog()
        assertEquals("node22", catalog.bases[0].name)
        assertEquals("demo", catalog.seeds[0].name)
        assertEquals("K", catalog.env[0].key)
        server.shutdown()
    }

    @Test
    fun createAgentPostsCatalogChoice() = runTest {
        val server = MockWebServer()
        server.enqueue(MockResponse().setBody("""{"id":"a2","name":"Two","state":"stopped"}"""))
        server.start()
        val created = newApi(server).createAgent("Two", "node22", "demo")
        assertEquals("a2", created.id)
        val request = server.takeRequest()
        assertEquals("/api/v1/agents", request.path)
        val body = request.body.readUtf8()
        assertTrue(body.contains("\"base\":\"node22\""))
        assertTrue(body.contains("\"seed\":\"demo\""))
        server.shutdown()
    }

    @Test
    fun enrollPostsPubkeyAndParsesResponse() = runTest {
        val server = MockWebServer()
        server.enqueue(MockResponse().setBody("""{"assigned_ip":"10.77.0.2","gateway_ip":"10.77.0.1"}"""))
        server.start()
        val resp = newApi(server).enroll("PUBKEYB64")
        assertEquals("10.77.0.2", resp.assignedIp)
        assertEquals("10.77.0.1", resp.gatewayIp)
        val request = server.takeRequest()
        assertEquals("/api/v1/enroll", request.path)
        assertTrue(request.body.readUtf8().contains("\"client_pubkey\":\"PUBKEYB64\""))
        server.shutdown()
    }

    @Test
    fun errorsPropagateAsFailures() = runTest {
        val server = MockWebServer()
        server.enqueue(MockResponse().setResponseCode(401).setBody("""{"message":"expired"}"""))
        server.start()
        val result = runCatching { newApi(server).listAgents() }
        assertTrue(result.isFailure)
        server.shutdown()
    }
}
