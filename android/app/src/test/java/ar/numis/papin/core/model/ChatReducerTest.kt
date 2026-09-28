package ar.numis.papin.core.model

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.putJsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatReducerTest {
    private fun update(vararg pairs: Pair<String, String>): ChatEvent.Update {
        val update = buildJsonObject {
            putJsonObject("content") {
                put("type", "text")
                put("text", pairs.toMap()["text"].orEmpty())
            }
            pairs.forEach { (k, v) -> if (k != "text") put(k, v) }
        }
        return ChatEvent.Update(update)
    }

    private fun stateWithSession() = ChatState(sessionId = "s1")

    @Test
    fun thinkingAndMessageStreamCoalesce() {
        var s = stateWithSession()
        s = ChatReducer.reduce(s, update("sessionUpdate" to "agent_thought_chunk", "text" to "hmm"))
        s = ChatReducer.reduce(s, update("sessionUpdate" to "agent_message_chunk", "text" to "par"))
        s = ChatReducer.reduce(s, update("sessionUpdate" to "agent_message_chunk", "text" to "tial"))
        assertEquals(
            listOf(
                ChatItem.Thinking("hmm"),
                ChatItem.AgentText("partial"),
            ),
            s.items,
        )
    }

    @Test
    fun toolCallCardUpdatesByIdAndToggles() {
        var s = stateWithSession()
        s = ChatReducer.reduce(
            s,
            ChatEvent.Update(
                buildJsonObject {
                    put("sessionUpdate", "tool_call")
                    put("toolCallId", "c1")
                    put("title", "cargo test")
                },
            ),
        )
        s = ChatReducer.reduce(
            s,
            ChatEvent.Update(
                buildJsonObject {
                    put("sessionUpdate", "tool_call_update")
                    put("toolCallId", "c1")
                    put("status", "completed")
                },
            ),
        )
        val card = s.items[0] as ChatItem.ToolCall
        assertEquals("completed", card.status)
        s = ChatReducer.toggle(s, 0)
        assertTrue((s.items[0] as ChatItem.ToolCall).expanded)
    }

    @Test
    fun planChecklistRendersCompletion() {
        val s = ChatReducer.reduce(
            stateWithSession(),
            ChatEvent.Update(
                buildJsonObject {
                    putJsonArray("plan") {
                        add(buildJsonObject { put("content", "one"); put("status", "completed") })
                        add(buildJsonObject { put("content", "two"); put("status", "in_progress") })
                    }
                    put("sessionUpdate", "plan")
                },
            ),
        )
        assertEquals(
            ChatItem.Plan(listOf(true to "one", false to "two")),
            s.items[0],
        )
    }

    @Test
    fun promptResponseClearsInFlight() {
        var s = ChatReducer.promptSubmitted(stateWithSession(), "hello")
        assertTrue(s.inFlight)
        assertFalse(s.canSend)
        s = ChatReducer.reduce(
            s,
            ChatEvent.Response(
                AcpEnvelope.response(
                    kotlinx.serialization.json.JsonPrimitive(1),
                    buildJsonObject { put("stopReason", "end_turn") },
                ),
            ),
        )
        assertFalse(s.inFlight)
        assertTrue(s.canSend)
    }

    @Test
    fun cancelAndBusyErrorsFreeInputWithNotice() {
        var s = ChatReducer.promptSubmitted(stateWithSession(), "x")
        s = ChatReducer.reduce(
            s,
            ChatEvent.Response(
                AcpEnvelope.errorResponse(kotlinx.serialization.json.JsonPrimitive(1), -32800, "Turn cancelled"),
            ),
        )
        assertFalse(s.inFlight)
        assertTrue(s.items.last() is ChatItem.StatusLine)

        var busy = ChatReducer.promptSubmitted(stateWithSession(), "y")
        busy = ChatReducer.reduce(
            busy,
            ChatEvent.Response(
                AcpEnvelope.errorResponse(kotlinx.serialization.json.JsonPrimitive(2), -32000, "turn.agent_busy"),
            ),
        )
        assertFalse(busy.inFlight)
    }

    @Test
    fun permissionReverseRequestBecomesPendingQuestion() {
        val env = AcpEnvelope(
            id = kotlinx.serialization.json.JsonPrimitive("perm-1"),
            sessionId = "s1",
            method = "session/request_permission",
            params = buildJsonObject {
                putJsonObject("toolCall") { put("title", "rm -rf x") }
                putJsonArray("options") {
                    add(buildJsonObject { put("kind", "allow_once") })
                    add(buildJsonObject { put("kind", "reject_once") })
                }
            },
        )
        val s = ChatReducer.reduce(stateWithSession(), ChatEvent.ReverseRequest(env))
        val q = s.pendingQuestion
        assertEquals("perm-1", (q!!.requestId as kotlinx.serialization.json.JsonPrimitive).content)
        assertEquals(listOf("allow_once", "reject_once"), q.options)
        val cleared = ChatReducer.questionAnswered(s)
        assertNull(cleared.pendingQuestion)
    }

    @Test
    fun sessionChangeClearsTranscript() {
        var s = ChatReducer.promptSubmitted(stateWithSession(), "hi")
        s = ChatReducer.reduce(s, ChatEvent.SessionChanged("s2"))
        assertEquals("s2", s.sessionId)
        assertTrue(s.items.isEmpty())
    }
}
