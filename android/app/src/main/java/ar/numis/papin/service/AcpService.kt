package ar.numis.papin.service

import android.app.Service
import android.content.Intent
import android.os.Binder
import android.os.IBinder
import ar.numis.papin.PapinApp
import ar.numis.papin.core.acp.AcpClient
import ar.numis.papin.core.model.AcpEnvelope
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * Foreground service that holds the ACP WebSocket while the app is
 * backgrounded (PLAN §7). One service instance per agent connection; the UI
 * binds to it. Emits events into a StateFlow the chat screen collects.
 *
 * Reconnect healing mirrors the CLI: on WS drop, reconnect + initialize +
 * session/load so a question never stalls silently.
 */
class AcpService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var client: AcpClient? = null
    private var agentId: String? = null
    private var eventJob: Job? = null

    private val _events = MutableStateFlow<AcpClient.AcpEvent?>(null)
    val events: StateFlow<AcpClient.AcpEvent?> = _events

    private val _sessionId = MutableStateFlow<String?>(null)
    val sessionId: StateFlow<String?> = _sessionId

    private val binder = LocalBinder()

    inner class LocalBinder : Binder() {
        fun service(): AcpService = this@AcpService
    }

    override fun onBind(intent: Intent?): IBinder = binder

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val id = intent?.getStringExtra(EXTRA_AGENT_ID)
        if (id != null && id != agentId) {
            connect(id)
        }
        return START_STICKY
    }

    fun connect(newAgentId: String) {
        disconnect()
        agentId = newAgentId
        val container = (application as PapinApp).container
        val c = container.acpClient(newAgentId) ?: return
        client = c
        if (android.os.Build.VERSION.SDK_INT >= 29) {
            startForeground(
                NotificationHelper.FG_NOTIFICATION_ID,
                NotificationHelper.foregroundNotification(this, newAgentId),
                android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
        } else {
            startForeground(
                NotificationHelper.FG_NOTIFICATION_ID,
                NotificationHelper.foregroundNotification(this, newAgentId),
            )
        }
        eventJob = scope.launch {
            c.eventFlow.collect { event ->
                _events.value = event
                when (event) {
                    is AcpClient.AcpEvent.ReverseRequest -> {
                        val session = event.envelope.sessionId
                        val title = event.envelope.params
                            ?.let { it as? JsonObject }
                            ?.get("toolCall")
                            ?.let { it as? JsonObject }
                            ?.get("title")
                            ?.jsonPrimitive?.content
                            ?: "the agent has a question"
                        NotificationHelper.questionNotification(
                            this@AcpService, newAgentId, session, title,
                        )
                    }
                    is AcpClient.AcpEvent.UntrackedResponse -> {
                        // In-flight prompt ended (or died): surface a turn-done
                        // notification when backgrounded.
                        if (event.envelope.error == null) {
                            NotificationHelper.turnDoneNotification(
                                this@AcpService, newAgentId, _sessionId.value,
                            )
                        }
                    }
                    is AcpClient.AcpEvent.Disconnected -> heal()
                    else -> Unit
                }
            }
        }
        c.connect()
    }

    fun disconnect() {
        eventJob?.cancel()
        eventJob = null
        client?.disconnect()
        client = null
    }

    /** Reconnect + re-initialize + session/load (§7 reconnect healing). */
    private fun heal() {
        val id = agentId ?: return
        val c = client ?: return
        scope.launch {
            _events.value = AcpClient.AcpEvent.Notification(
                AcpEnvelope.notification("papin/reconnecting", null),
            )
            c.connect()
            runCatching {
                c.request(
                    "initialize",
                    buildJsonObject {
                        put("protocolVersion", 1)
                        put("clientCapabilities", buildJsonObject {})
                        put("clientInfo", buildJsonObject {
                            put("name", "papin-android")
                            put("version", "0.1.0")
                        })
                    },
                )
                c.notify("initialized", buildJsonObject {})
                _sessionId.value?.let { sid ->
                    c.request("session/load", buildJsonObject { put("sessionId", sid) })
                }
            }
        }
    }

    suspend fun newSession(): String? {
        val c = client ?: return null
        val resp = c.request("session/new", buildJsonObject { put("cwd", "/workspace") })
        val sid = (resp.result as? JsonObject)?.get("sessionId")?.jsonPrimitive?.content
        _sessionId.value = sid
        return sid
    }

    suspend fun loadSession(sid: String) {
        val c = client ?: return
        c.request("session/load", buildJsonObject { put("sessionId", sid) })
        _sessionId.value = sid
    }

    fun clientOrNull(): AcpClient? = client

    override fun onDestroy() {
        disconnect()
        super.onDestroy()
    }

    companion object {
        const val EXTRA_AGENT_ID = "agent_id"

        fun startIntent(context: android.content.Context, agentId: String): Intent =
            Intent(context, AcpService::class.java).putExtra(EXTRA_AGENT_ID, agentId)
    }
}
