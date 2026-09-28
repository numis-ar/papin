package ar.numis.papin.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import ar.numis.papin.core.model.AgentDto
import ar.numis.papin.core.model.SessionInfoDto
import ar.numis.papin.di.AppContainer
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

class SessionsViewModel(private val container: AppContainer) : ViewModel() {
    private val _agents = MutableStateFlow<List<AgentDto>>(emptyList())
    val agents: StateFlow<List<AgentDto>> = _agents

    private val _sessions = MutableStateFlow<Map<String, List<SessionInfoDto>>>(emptyMap())
    val sessions: StateFlow<Map<String, List<SessionInfoDto>>> = _sessions

    private val _loading = MutableStateFlow(false)
    val loading: StateFlow<Boolean> = _loading

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    init {
        refresh()
    }

    fun refresh() {
        val api = container.gatewayApi ?: return
        viewModelScope.launch {
            _loading.value = true
            runCatching { api.listAgents() }
                .onSuccess { agents -> _agents.value = agents }
                .onFailure { _error.value = it.message }
            _loading.value = false
        }
    }

    /** session/list is a connection-scoped ACP method: fetched per agent over a short-lived WS. */
    fun loadSessionsFor(agentId: String) {
        val url = container.settings.gatewayUrl ?: return
        val token = container.settings.deviceToken ?: return
        viewModelScope.launch {
            val client = container.acpClient(agentId) ?: return@launch
            client.connect()
            runCatching {
                val init = client.request(
                    "initialize",
                    kotlinx.serialization.json.buildJsonObject {
                        put("protocolVersion", 1)
                        put("clientCapabilities", kotlinx.serialization.json.buildJsonObject {})
                        put("clientInfo", kotlinx.serialization.json.buildJsonObject {
                            put("name", "papin-android")
                            put("version", "0.1.0")
                        })
                    },
                )
                if (init.error != null) error("initialize failed: ${init.error.message}")
                client.notify("initialized", kotlinx.serialization.json.buildJsonObject {})
                client.request("session/list", kotlinx.serialization.json.buildJsonObject {})
            }.onSuccess { listResp ->
                val entries = (listResp.result as? kotlinx.serialization.json.JsonObject)
                    ?.get("sessions") as? kotlinx.serialization.json.JsonArray
                val parsed = entries?.mapNotNull { el ->
                    (el as? kotlinx.serialization.json.JsonObject)?.let { o ->
                        SessionInfoDto(
                            sessionId = o["sessionId"]?.toString()?.trim('"').orEmpty(),
                            title = o["title"]?.toString()?.trim('"').orEmpty(),
                        )
                    }
                }.orEmpty()
                _sessions.value = _sessions.value + (agentId to parsed)
            }.onFailure { _error.value = it.message }
            client.disconnect()
        }
    }
}

/** Sessions screen: session/list per agent; tap resumes via session/load on the chat screen. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionsScreen(container: AppContainer, onOpenChat: (String) -> Unit) {
    val model: SessionsViewModel = viewModel(factory = viewModelFactory { SessionsViewModel(container) })
    val agents by model.agents.collectAsState()
    val sessions by model.sessions.collectAsState()
    val loading by model.loading.collectAsState()
    val error by model.error.collectAsState()

    Scaffold(topBar = { TopAppBar(title = { Text("Sessions") }) }) { padding ->
        PullToRefreshBox(
            isRefreshing = loading,
            onRefresh = { model.refresh() },
            modifier = Modifier.fillMaxSize().padding(padding),
        ) {
            LazyColumn(
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                agents.forEach { agent ->
                    item {
                        Text(
                            agent.name.ifEmpty { agent.id },
                            style = MaterialTheme.typography.titleSmall,
                            color = MaterialTheme.colorScheme.primary,
                            modifier = Modifier
                                .clickable { model.loadSessionsFor(agent.id) }
                                .padding(vertical = 4.dp),
                        )
                    }
                    val list = sessions[agent.id]
                    if (list == null) {
                        item {
                            Text(
                                "tap to load sessions",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.clickable { model.loadSessionsFor(agent.id) },
                            )
                        }
                    } else if (list.isEmpty()) {
                        item { Text("no sessions", style = MaterialTheme.typography.bodySmall) }
                    } else {
                        items(list, key = { "${agent.id}:${it.sessionId}" }) { session ->
                            Column(
                                Modifier
                                    .fillMaxWidth()
                                    .clickable { onOpenChat(agent.id) }
                                    .padding(vertical = 8.dp),
                            ) {
                                Text(session.title.ifEmpty { session.sessionId }, style = MaterialTheme.typography.bodyLarge)
                                Text(session.sessionId, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                            }
                        }
                    }
                }
                error?.let {
                    item {
                        Spacer(Modifier.height(8.dp))
                        Text(it, color = MaterialTheme.colorScheme.error)
                    }
                }
            }
        }
    }
}
