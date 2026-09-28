package ar.numis.papin.core.acp

import ar.numis.papin.core.model.AcpEnvelope
import ar.numis.papin.core.model.idKeyOf
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonPrimitive
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * ACP client over the gateway WebSocket (§7). One connection per agent.
 * Responses to our requests are demuxed by JSON-RPC id; everything else
 * (session/update notifications, agent→client reverse-RPC, in-flight prompt
 * responses) is emitted as [AcpEvent].
 */
class AcpClient(
    private val httpUrl: String,
    private val agentId: String,
    private val token: String,
    private val okHttpClient: OkHttpClient,
    private val scope: CoroutineScope,
) {
    sealed interface AcpEvent {
        data class Notification(val envelope: AcpEnvelope) : AcpEvent
        data class UntrackedResponse(val envelope: AcpEnvelope) : AcpEvent
        data class ReverseRequest(val envelope: AcpEnvelope) : AcpEvent
        data object Disconnected : AcpEvent
    }

    private val events = MutableSharedFlow<AcpEvent>(extraBufferCapacity = 256)
    val eventFlow: SharedFlow<AcpEvent> = events

    private val pending = ConcurrentHashMap<String, CompletableDeferred<AcpEnvelope>>()
    private val nextId = AtomicLong(1)
    @Volatile private var socket: WebSocket? = null

    fun connect(): Job = scope.launch(Dispatchers.IO) {
        val wsUrl = httpUrl
            .replaceFirst("http://", "ws://")
            .replaceFirst("https://", "wss://")
            .trimEnd('/') + "/acp/agents/$agentId"
        val request = Request.Builder()
            .url(wsUrl)
            .header("Authorization", "Bearer $token")
            .build()
        socket = okHttpClient.newWebSocket(request, listener)
    }

    fun disconnect() {
        socket?.close(1000, "client done")
        socket = null
    }

    fun newRequestId(): JsonElement = JsonPrimitive(nextId.getAndIncrement())

    /** Send a fire-and-forget notification (e.g. initialized). */
    fun notify(method: String, params: JsonElement? = null) {
        sendEnvelope(AcpEnvelope.notification(method, params))
    }

    /** Send a request and await its response. */
    suspend fun request(method: String, params: JsonElement? = null): AcpEnvelope {
        val id = newRequestId()
        return requestWithId(id, method, params)
    }

    suspend fun requestWithId(id: JsonElement, method: String, params: JsonElement? = null): AcpEnvelope {
        val deferred = CompletableDeferred<AcpEnvelope>()
        val key = idKeyOf(id)
        pending[key] = deferred
        sendEnvelope(AcpEnvelope.request(id, method, params))
        return try {
            deferred.await()
        } finally {
            pending.remove(key)
        }
    }

    /** Answer an agent reverse-RPC. */
    fun respond(id: JsonElement, result: JsonElement) {
        sendEnvelope(AcpEnvelope.response(id, result))
    }

    private fun sendEnvelope(env: AcpEnvelope) {
        val text = AcpEnvelope.encode(env)
        socket?.send(text) ?: throw IllegalStateException("not connected")
    }

    private val listener = object : WebSocketListener() {
        override fun onMessage(webSocket: WebSocket, text: String) {
            val env = runCatching { AcpEnvelope.decode(text) }.getOrNull() ?: return
            when {
                env.isResponse -> {
                    val deferred = pending.remove(env.idKey)
                    if (deferred != null) deferred.complete(env)
                    else scope.launch { events.emit(AcpEvent.UntrackedResponse(env)) }
                }
                env.isRequest -> scope.launch { events.emit(AcpEvent.ReverseRequest(env)) }
                else -> scope.launch { events.emit(AcpEvent.Notification(env)) }
            }
        }

        override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
            failAll()
            scope.launch { events.emit(AcpEvent.Disconnected) }
        }

        override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
            failAll()
            scope.launch { events.emit(AcpEvent.Disconnected) }
        }
    }

    private fun failAll() {
        pending.values.forEach { it.completeExceptionally(IllegalStateException("connection closed")) }
        pending.clear()
    }
}
