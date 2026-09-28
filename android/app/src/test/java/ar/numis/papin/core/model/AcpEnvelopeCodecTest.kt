package ar.numis.papin.core.model

import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AcpEnvelopeCodecTest {
    @Test
    fun requestRoundTrip() {
        val env = AcpEnvelope.request(
            JsonPrimitive(1),
            "initialize",
            buildJsonObject { put("protocolVersion", 1) },
        )
        val text = AcpEnvelope.encode(env)
        assertTrue(text.contains("\"method\":\"initialize\""))
        assertTrue(text.contains("\"id\":1"))
        assertFalse(text.contains("sessionId"))
        assertFalse(text.contains("result"))
        assertEquals(env, AcpEnvelope.decode(text))
    }

    @Test
    fun notificationHasNoId() {
        val env = AcpEnvelope.notification("initialized", buildJsonObject {})
        val text = AcpEnvelope.encode(env)
        assertFalse(text.contains("\"id\""))
        assertTrue(env.isNotification)
        assertFalse(env.isResponse)
    }

    @Test
    fun stringIdsAndSessionIdRoundTrip() {
        val env = AcpEnvelope(
            id = JsonPrimitive("perm-1"),
            sessionId = "s1",
            method = "session/request_permission",
            params = buildJsonObject { put("sessionId", "s1") },
        )
        val decoded = AcpEnvelope.decode(AcpEnvelope.encode(env))
        assertEquals("perm-1", (decoded.id as JsonPrimitive).content)
        assertEquals("s1", decoded.sessionId)
        assertEquals("session/request_permission", decoded.method)
    }

    @Test
    fun responseWithError() {
        val env = AcpEnvelope.errorResponse(JsonPrimitive(3), -32800, "Turn cancelled")
        val decoded = AcpEnvelope.decode(AcpEnvelope.encode(env))
        assertTrue(decoded.isResponse)
        assertEquals(-32800, decoded.error?.code)
        assertEquals("Turn cancelled", decoded.error?.message)
        assertNull(decoded.result)
    }

    @Test
    fun unknownFieldsAreIgnored() {
        val decoded = AcpEnvelope.decode(
            """{"id":1,"method":"initialize","params":{},"jsonrpc":"2.0","future":"x"}""",
        )
        assertEquals("initialize", decoded.method)
    }

    @Test
    fun updateKindAndTextExtraction() {
        val env = AcpEnvelope.notification(
            "session/update",
            buildJsonObject {
                put("sessionId", "s1")
                put(
                    "update",
                    buildJsonObject {
                        put("sessionUpdate", "agent_message_chunk")
                        put("content", buildJsonObject { put("type", "text"); put("text", "hello") })
                    },
                )
            },
        )
        assertEquals("agent_message_chunk", env.sessionUpdateKind())
        assertEquals("hello", env.textContent())
    }
}
