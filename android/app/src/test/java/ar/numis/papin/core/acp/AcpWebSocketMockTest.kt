package ar.numis.papin.core.acp

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.OkHttpClient
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AcpWebSocketMockTest {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    @Test
    fun requestResponseDemuxAndNotificationFlow() = runBlocking {
        val server = MockWebServer()
        server.enqueue(
            MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
                override fun onMessage(webSocket: WebSocket, text: String) {
                    when {
                        text.contains("\"initialize\"") -> {
                            webSocket.send("""{"id":1,"result":{"protocolVersion":1,"capabilities":{}}}""")
                        }
                        text.contains("\"session/prompt\"") -> {
                            webSocket.send("""{"sessionId":"s1","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}}}""")
                            webSocket.send("""{"id":"perm-1","sessionId":"s1","method":"session/request_permission","params":{}}""")
                            webSocket.send("""{"id":2,"sessionId":"s1","result":{"stopReason":"end_turn"}}""")
                        }
                    }
                }
            }),
        )
        server.start()

        val client = AcpClient(
            httpUrl = server.url("/").toString().removeSuffix("/"),
            agentId = "a1",
            token = "tok",
            okHttpClient = OkHttpClient(),
            scope = scope,
        )
        client.connect()

        // initialize round-trip.
        val init = withTimeout(5_000) {
            client.request("initialize", buildJsonObject { put("protocolVersion", 1) })
        }
        assertEquals(1, (init.result as? kotlinx.serialization.json.JsonObject)
            ?.get("protocolVersion")?.let { (it as JsonPrimitive).content.toInt() })

        // Bearer auth hit the WS endpoint.
        val recorded = server.takeRequest()
        assertEquals("/acp/agents/a1", recorded.path)
        assertEquals("Bearer tok", recorded.getHeader("Authorization"))

        // prompt (sent untracked) → update notification + reverse request +
        // untracked response, all via the SharedFlow.
        val promptId = client.newRequestId()
        val collected = mutableListOf<AcpClient.AcpEvent>()
        val collector = scope.launch {
            client.eventFlow.collect { e -> if (e != null) collected.add(e) }
        }
        val promptDeferred = scope.launch {
            client.requestWithId(promptId, "session/prompt", buildJsonObject { put("sessionId", "s1") })
        }
        withTimeout(5_000) {
            while (collected.count { it is AcpClient.AcpEvent.UntrackedResponse } == 0 &&
                collected.none { it is AcpClient.AcpEvent.ReverseRequest }
            ) {
                kotlinx.coroutines.delay(20)
            }
        }
        assertTrue(collected.any { it is AcpClient.AcpEvent.Notification })
        assertTrue(collected.any { it is AcpClient.AcpEvent.ReverseRequest })
        collector.cancel()
        promptDeferred.cancel()
        client.disconnect()
        server.shutdown()
    }

    @Test
    fun disconnectEmitsDisconnectedEvent() = runBlocking {
        val server = MockWebServer()
        server.enqueue(
            MockResponse().withWebSocketUpgrade(object : WebSocketListener() {
                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    webSocket.close(code, reason) // answer the close handshake
                }
            }),
        )
        server.start()
        val client = AcpClient(
            httpUrl = server.url("/").toString().removeSuffix("/"),
            agentId = "a1",
            token = "t",
            okHttpClient = OkHttpClient(),
            scope = scope,
        )
        client.connect()
        server.takeRequest() // wait until the upgrade request arrived
        val events = mutableListOf<AcpClient.AcpEvent>()
        val collector = scope.launch {
            client.eventFlow.collect { e -> if (e != null) events.add(e) }
        }
        client.disconnect()
        withTimeout(5_000) {
            while (events.none { it is AcpClient.AcpEvent.Disconnected }) {
                kotlinx.coroutines.delay(20)
            }
        }
        assertNotNull(events.filterIsInstance<AcpClient.AcpEvent.Disconnected>().firstOrNull())
        collector.cancel()
        server.shutdown()
    }
}
