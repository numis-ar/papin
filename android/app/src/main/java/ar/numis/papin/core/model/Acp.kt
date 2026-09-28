package ar.numis.papin.core.model

import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.descriptors.buildClassSerialDescriptor
import kotlinx.serialization.descriptors.element
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonDecoder
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonEncoder
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

/**
 * ACP v1 JSON-RPC envelope — field names exactly as on the wire (`id`,
 * `sessionId`, `method`, `params`, `result`, `error`). `id` may be a number,
 * string, or absent (notifications); modeled as [JsonElement]? so both work.
 */
@Serializable(with = AcpEnvelopeSerializer::class)
data class AcpEnvelope(
    val id: JsonElement? = null,
    val sessionId: String? = null,
    val method: String? = null,
    val params: JsonElement? = null,
    val result: JsonElement? = null,
    val error: AcpError? = null,
) {
    val isRequest: Boolean get() = method != null && id != null
    val isNotification: Boolean get() = method != null && id == null
    val isResponse: Boolean get() = method == null && id != null

    /** Stable routing key for the JSON-RPC id. */
    val idKey: String get() = id?.let { idKeyOf(it) } ?: ""

    companion object {
        val json = Json { ignoreUnknownKeys = true; encodeDefaults = false }

        fun request(id: JsonElement, method: String, params: JsonElement? = null): AcpEnvelope =
            AcpEnvelope(id = id, method = method, params = params)

        fun notification(method: String, params: JsonElement? = null): AcpEnvelope =
            AcpEnvelope(method = method, params = params)

        fun response(id: JsonElement, result: JsonElement): AcpEnvelope =
            AcpEnvelope(id = id, result = result)

        fun errorResponse(id: JsonElement, code: Int, message: String): AcpEnvelope =
            AcpEnvelope(id = id, error = AcpError(code, message))

        fun decode(text: String): AcpEnvelope = json.decodeFromString(serializer(), text)

        fun encode(env: AcpEnvelope): String = json.encodeToString(serializer(), env)

        /** Numeric request ids for client→agent requests. */
        fun numberId(n: Long): JsonElement = JsonPrimitive(n)
    }
}

@Serializable
data class AcpError(
    val code: Int,
    val message: String,
    val data: JsonElement? = null,
)

/** Hand-rolled serializer so `sessionId` and optional-null omission match the wire exactly. */
object AcpEnvelopeSerializer : KSerializer<AcpEnvelope> {
    override val descriptor: SerialDescriptor = buildClassSerialDescriptor("AcpEnvelope") {
        element<JsonElement?>("id")
        element<String?>("sessionId")
        element<String?>("method")
        element<JsonElement?>("params")
        element<JsonElement?>("result")
        element<AcpError?>("error")
    }

    override fun deserialize(decoder: Decoder): AcpEnvelope {
        val input = decoder as JsonDecoder
        val obj = input.decodeJsonElement().jsonObject
        return AcpEnvelope(
            id = obj["id"],
            sessionId = obj.str("sessionId"),
            method = obj.str("method"),
            params = obj["params"],
            result = obj["result"],
            error = obj["error"]?.let { input.json.decodeFromJsonElement(AcpError.serializer(), it) },
        )
    }

    override fun serialize(encoder: Encoder, value: AcpEnvelope) {
        val output = encoder as JsonEncoder
        val obj = buildJsonObject {
            value.id?.let { put("id", it) }
            value.sessionId?.let { put("sessionId", it) }
            value.method?.let { put("method", it) }
            value.params?.let { if (it !is JsonNull) put("params", it) }
            value.result?.let { if (it !is JsonNull) put("result", it) }
            value.error?.let { put("error", output.json.encodeToJsonElement(AcpError.serializer(), it)) }
        }
        output.encodeJsonElement(obj)
    }
}

// Convenience accessors used across the app.
val AcpEnvelope.isTurnCancelledError: Boolean get() = error?.message?.contains("cancelled") == true
val AcpEnvelope.isAgentBusyError: Boolean get() = error?.message?.contains("agent_busy") == true

/** Extract `update.sessionUpdate` from a session/update notification. */
fun AcpEnvelope.sessionUpdateKind(): String? =
    if (method == "session/update")
        (params as? JsonObject)?.obj("update")?.str("sessionUpdate")
    else null

fun AcpEnvelope.textContent(): String =
    (params as? JsonObject)?.obj("update")?.obj("content")?.str("text") ?: ""

internal fun JsonObject.str(key: String): String? = (get(key) as? JsonPrimitive)?.contentOrNull
internal fun JsonObject.obj(key: String): JsonObject? = get(key) as? JsonObject

/** Stable map key for any JSON-RPC id value. */
fun idKeyOf(id: JsonElement): String = Json.encodeToString(JsonElement.serializer(), id)
