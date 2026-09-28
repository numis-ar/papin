package ar.numis.papin.core.model

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray

/** Transcript entries for the chat screen (§7: per-output-type components). */
sealed interface ChatItem {
    data class UserText(val text: String) : ChatItem
    data class Thinking(val text: String, val collapsed: Boolean = false) : ChatItem
    data class ToolCall(
        val toolCallId: String,
        val title: String,
        val status: String = "running",
        val detail: String = "",
        val expanded: Boolean = false,
    ) : ChatItem

    data class Plan(val items: List<Pair<Boolean, String>>) : ChatItem
    data class AgentText(val text: String, val streaming: Boolean = true) : ChatItem
    data class StatusLine(val text: String) : ChatItem
}

/** Reverse-RPC question from the agent (permission / elicitation). */
data class PendingQuestion(
    val requestId: JsonElement,
    val method: String, // session/request_permission | session/elicitation
    val title: String,
    val options: List<String>, // permission outcome kinds
)

sealed interface ChatEvent {
    /** A session/update notification. */
    data class Update(val update: JsonObject) : ChatEvent

    /** Response envelope with a matching id (prompt result, cancel ack, …). */
    data class Response(val envelope: AcpEnvelope) : ChatEvent

    data class ReverseRequest(val envelope: AcpEnvelope) : ChatEvent
    data class TurnEnded(val stopReason: String) : ChatEvent
    data class TurnFailed(val message: String) : ChatEvent
    data class SessionChanged(val sessionId: String?) : ChatEvent
    data class Status(val text: String) : ChatEvent
}

/**
 * Pure reducer: (state, event) → new state. The ViewModel owns side effects
 * (sending ACP requests); this class is unit-testable without Android.
 */
data class ChatState(
    val items: List<ChatItem> = emptyList(),
    val sessionId: String? = null,
    val inFlight: Boolean = false,
    val pendingQuestion: PendingQuestion? = null,
) {
    val canSend: Boolean get() = !inFlight && sessionId != null
}

object ChatReducer {
    fun reduce(state: ChatState, event: ChatEvent): ChatState = when (event) {
        is ChatEvent.Update -> applyUpdate(state, event.update)
        is ChatEvent.Response -> {
            if (state.inFlight && event.envelope.isResponse) {
                when {
                    event.envelope.isTurnCancelledError -> state.copy(
                        inFlight = false,
                        items = state.items + ChatItem.StatusLine("turn cancelled"),
                    )

                    event.envelope.isAgentBusyError -> state.copy(
                        inFlight = false,
                        items = state.items + ChatItem.StatusLine("agent is busy; wait for the current turn"),
                    )

                    event.envelope.error != null -> state.copy(
                        inFlight = false,
                        items = state.items + ChatItem.StatusLine("error: ${event.envelope.error.message}"),
                    )

                    else -> state.copy(
                        inFlight = false,
                        items = state.items.map {
                            if (it is ChatItem.AgentText && it.streaming) it.copy(streaming = false) else it
                        },
                    )
                }
            } else state
        }

        is ChatEvent.ReverseRequest -> applyReverseRequest(state, event.envelope)
        is ChatEvent.TurnEnded -> state.copy(
            inFlight = false,
            items = state.items.map {
                if (it is ChatItem.AgentText && it.streaming) it.copy(streaming = false) else it
            },
        )

        is ChatEvent.TurnFailed -> state.copy(
            inFlight = false,
            items = state.items + ChatItem.StatusLine("error: ${event.message}"),
        )

        is ChatEvent.SessionChanged -> state.copy(sessionId = event.sessionId, items = emptyList())
        is ChatEvent.Status -> state.copy(items = state.items + ChatItem.StatusLine(event.text))
    }

    /** A user prompt was accepted for sending. */
    fun promptSubmitted(state: ChatState, text: String): ChatState =
        state.copy(
            inFlight = true,
            items = state.items + ChatItem.UserText(text),
        )

    fun toggle(state: ChatState, index: Int): ChatState {
        val item = state.items.getOrNull(index) ?: return state
        val updated = when (item) {
            is ChatItem.Thinking -> item.copy(collapsed = !item.collapsed)
            is ChatItem.ToolCall -> item.copy(expanded = !item.expanded)
            else -> return state
        }
        return state.copy(items = state.items.toMutableList().also { it[index] = updated })
    }

    fun questionAnswered(state: ChatState): ChatState = state.copy(pendingQuestion = null)

    private fun applyUpdate(state: ChatState, update: JsonObject): ChatState {
        val kind = update["sessionUpdate"]?.jsonPrimitive?.contentOrNull ?: return state
        return when (kind) {
            "agent_thought_chunk" -> {
                val text = update.textOf("content")
                val last = state.items.lastOrNull()
                if (last is ChatItem.Thinking && !last.collapsed) {
                    state.replaceLast(last.copy(text = last.text + text))
                } else {
                    state.copy(items = state.items + ChatItem.Thinking(text))
                }
            }

            "agent_message_chunk" -> {
                val text = update.textOf("content")
                val last = state.items.lastOrNull()
                if (last is ChatItem.AgentText && last.streaming) {
                    state.replaceLast(last.copy(text = last.text + text))
                } else {
                    state.copy(items = state.items + ChatItem.AgentText(text))
                }
            }

            "tool_call" -> state.copy(
                items = state.items + ChatItem.ToolCall(
                    toolCallId = update["toolCallId"]?.jsonPrimitive?.contentOrNull.orEmpty(),
                    title = update["title"]?.jsonPrimitive?.contentOrNull ?: "tool call",
                ),
            )

            "tool_call_update" -> {
                val id = update["toolCallId"]?.jsonPrimitive?.contentOrNull.orEmpty()
                val status = update["status"]?.jsonPrimitive?.contentOrNull ?: "completed"
                val content = update["content"]?.toString().orEmpty()
                state.copy(
                    items = state.items.map {
                        if (it is ChatItem.ToolCall && it.toolCallId == id) {
                            it.copy(status = status, detail = if (content.isNotEmpty()) content else it.detail)
                        } else it
                    },
                )
            }

            "plan" -> {
                val entries = (update["plan"] as? JsonArray)?.mapNotNull { entry ->
                    (entry as? JsonObject)?.let { o ->
                        val done = o["status"]?.jsonPrimitive?.contentOrNull == "completed"
                        val content = o["content"]?.jsonPrimitive?.contentOrNull ?: return@let null
                        done to content
                    }
                }.orEmpty()
                state.copy(items = state.items + ChatItem.Plan(entries))
            }

            else -> state
        }
    }

    private fun applyReverseRequest(state: ChatState, env: AcpEnvelope): ChatState = when (env.method) {
        "session/request_permission" -> {
            val params = env.params?.let { it as? JsonObject }
            val tool = params?.get("toolCall") as? JsonObject
            val title = tool?.get("title")?.jsonPrimitive?.contentOrNull ?: "permission requested"
            val options = (params?.get("options") as? JsonArray)
                ?.mapNotNull { (it as? JsonObject)?.get("kind")?.jsonPrimitive?.contentOrNull }
                ?.takeIf { it.isNotEmpty() }
                ?: listOf("allow_once", "reject_once")
            state.copy(
                pendingQuestion = PendingQuestion(
                    requestId = env.id ?: JsonPrimitive(""),
                    method = "session/request_permission",
                    title = title,
                    options = options,
                ),
            )
        }

        "session/elicitation" -> state.copy(
            pendingQuestion = PendingQuestion(
                requestId = env.id ?: JsonPrimitive(""),
                method = "session/elicitation",
                title = "the agent has a question",
                options = emptyList(),
            ),
        )

        else -> state
    }

    private fun ChatState.replaceLast(item: ChatItem): ChatState =
        copy(items = items.toMutableList().also { it[it.size - 1] = item })

    private fun JsonObject.textOf(key: String): String =
        (get(key) as? JsonObject)?.get("text")?.jsonPrimitive?.contentOrNull ?: ""

    /** Build the session/cancel request params. */
    fun cancelParams(sessionId: String): JsonObject = buildJsonObject {
        put("sessionId", sessionId)
    }

    /** Build the session/prompt params. */
    fun promptParams(sessionId: String, text: String): JsonObject = buildJsonObject {
        put("sessionId", sessionId)
        putJsonArray("prompt") {
            add(buildJsonObject {
                put("type", "text")
                put("text", text)
            })
        }
    }
}
