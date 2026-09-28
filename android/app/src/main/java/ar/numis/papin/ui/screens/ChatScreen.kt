package ar.numis.papin.ui.screens

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.IBinder
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import ar.numis.papin.core.acp.AcpClient
import ar.numis.papin.core.model.AcpEnvelope
import ar.numis.papin.core.model.ChatEvent
import ar.numis.papin.core.model.ChatItem
import ar.numis.papin.core.model.ChatReducer
import ar.numis.papin.core.model.ChatState
import ar.numis.papin.core.model.PendingQuestion
import ar.numis.papin.di.AppContainer
import ar.numis.papin.service.AcpService
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

class ChatViewModel(private val container: AppContainer, private val agentId: String) : ViewModel() {
    private val _state = MutableStateFlow(ChatState())
    val state: StateFlow<ChatState> = _state

    private val _input = MutableStateFlow("")
    val input: StateFlow<String> = _input

    private var client: AcpClient? = null

    fun bind(service: AcpService) {
        client = service.clientOrNull()
        viewModelScope.launch {
            service.events.collect { event ->
                if (event == null) return@collect
                when (event) {
                    is AcpClient.AcpEvent.Notification -> {
                        if (event.envelope.method == "papin/reconnecting") {
                            reduce(ChatEvent.Status("connection lost; reconnecting…"))
                        } else {
                            reduce(ChatEvent.Update(event.envelope.params as? JsonObject ?: return@collect))
                        }
                    }

                    is AcpClient.AcpEvent.UntrackedResponse ->
                        reduce(ChatEvent.Response(event.envelope))

                    is AcpClient.AcpEvent.ReverseRequest ->
                        reduce(ChatEvent.ReverseRequest(event.envelope))

                    is AcpClient.AcpEvent.Disconnected ->
                        reduce(ChatEvent.Status("connection lost; reconnecting…"))

                    else -> Unit
                }
            }
        }
        viewModelScope.launch {
            service.sessionId.collect { sid ->
                if (sid != null && sid != _state.value.sessionId) {
                    reduce(ChatEvent.SessionChanged(sid))
                }
            }
        }
    }

    fun startSession(service: AcpService, resume: String?) {
        viewModelScope.launch {
            if (resume != null) {
                service.loadSession(resume)
            } else if (service.sessionId.value == null) {
                service.newSession()
            }
        }
    }

    fun onInput(text: String) {
        _input.value = text
    }

    fun send(service: AcpService) {
        val text = _input.value.trim()
        val sid = _state.value.sessionId ?: return
        if (text.isEmpty() || !_state.value.canSend) return
        val id = client?.newRequestId() ?: return
        val current = client ?: return
        _state.value = ChatReducer.promptSubmitted(_state.value, text)
        _input.value = ""
        viewModelScope.launch {
            runCatching {
                current.requestWithId(
                    id,
                    "session/prompt",
                    ChatReducer.promptParams(sid, text),
                )
            }.onSuccess { resp ->
                reduce(ChatEvent.Response(resp))
            }.onFailure { e ->
                reduce(ChatEvent.TurnFailed(e.message ?: "send failed"))
            }
        }
    }

    fun cancel(service: AcpService) {
        val sid = _state.value.sessionId ?: return
        viewModelScope.launch {
            service.clientOrNull()?.request("session/cancel", ChatReducer.cancelParams(sid))
            reduce(ChatEvent.Status("cancelling…"))
        }
    }

    fun answerQuestion(option: String) {
        val question = _state.value.pendingQuestion ?: return
        val result = buildJsonObject { put("outcome", option) }
        client?.respond(question.requestId, result)
        _state.value = ChatReducer.questionAnswered(_state.value)
    }

    fun toggle(index: Int) {
        _state.value = ChatReducer.toggle(_state.value, index)
    }

    private fun reduce(event: ChatEvent) {
        _state.value = ChatReducer.reduce(_state.value, event)
    }
}

/** Conversation-like chat screen (§7): bubbles, thinking/tool/plan cards. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChatScreen(
    container: AppContainer,
    agentId: String,
    initialSessionId: String?,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    val model: ChatViewModel = viewModel(
        factory = viewModelFactory { ChatViewModel(container, agentId) },
    )
    val state by model.state.collectAsState()
    val input by model.input.collectAsState()
    val listState = rememberLazyListState()
    var service by remember { mutableStateOf<AcpService?>(null) }

    // Bind to the foreground ACP service; it survives backgrounding.
    DisposableEffect(agentId) {
        var bound: AcpService? = null
        val connection = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
                val s = (binder as? AcpService.LocalBinder)?.service() ?: return
                bound = s
                service = s
                model.bind(s)
                model.startSession(s, initialSessionId)
            }

            override fun onServiceDisconnected(name: ComponentName?) {
                service = null
            }
        }
        context.startForegroundService(AcpService.startIntent(context, agentId))
        context.bindService(
            AcpService.startIntent(context, agentId),
            connection,
            Context.BIND_AUTO_CREATE,
        )
        onDispose {
            bound?.let { context.unbindService(connection) }
        }
    }

    LaunchedEffect(state.items.size) {
        if (state.items.isNotEmpty()) listState.animateScrollToItem(state.items.size - 1)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(agentId) },
                navigationIcon = {
                    IconButton(onClick = onBack) { Text("←") }
                },
            )
        },
    ) { padding ->
        Column(Modifier.fillMaxSize().padding(padding)) {
            LazyColumn(
                state = listState,
                modifier = Modifier.weight(1f).fillMaxWidth(),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                itemsIndexed(state.items) { index, item ->
                    ChatItemView(item, onToggle = { model.toggle(index) })
                }
            }
            Row(
                modifier = Modifier.fillMaxWidth().padding(8.dp),
                verticalAlignment = Alignment.Bottom,
            ) {
                OutlinedTextField(
                    value = input,
                    onValueChange = model::onInput,
                    modifier = Modifier.weight(1f),
                    placeholder = { Text("Message the agent…") },
                    maxLines = 5,
                )
                Spacer(Modifier.width(8.dp))
                when {
                    state.inFlight -> Button(onClick = { service?.let(model::cancel) }) {
                        Text("Stop")
                    }
                    else -> Button(
                        onClick = { service?.let(model::send) },
                        enabled = state.canSend && input.isNotBlank(),
                    ) { Text("Send") }
                }
            }
        }
    }

    // Full-screen question dialog (§7).
    state.pendingQuestion?.let { question ->
        QuestionDialog(question = question, onAnswer = model::answerQuestion)
    }
}

@Composable
private fun ChatItemView(item: ChatItem, onToggle: () -> Unit) {
    when (item) {
        is ChatItem.UserText -> Surface(
            shape = MaterialTheme.shapes.medium,
            color = MaterialTheme.colorScheme.primaryContainer,
            modifier = Modifier.fillMaxWidth(0.85f),
        ) {
            Text(item.text, Modifier.padding(12.dp))
        }

        is ChatItem.Thinking -> Card(
            onClick = onToggle,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Column(Modifier.padding(12.dp)) {
                Text(
                    if (item.collapsed) "▶ thinking (tap to expand)" else "▼ thinking",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (!item.collapsed) {
                    Text(item.text, style = MaterialTheme.typography.bodyMedium, fontStyle = FontStyle.Italic)
                }
            }
        }

        is ChatItem.ToolCall -> Card(
            onClick = onToggle,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Column(Modifier.padding(12.dp)) {
                Text(
                    "${if (item.expanded) "▶" else "▼"} [${item.status}] ${item.title}",
                    style = MaterialTheme.typography.labelLarge,
                    color = when (item.status) {
                        "completed" -> MaterialTheme.colorScheme.primary
                        else -> MaterialTheme.colorScheme.tertiary
                    },
                )
                if (item.expanded && item.detail.isNotEmpty()) {
                    Text(item.detail, style = MaterialTheme.typography.bodySmall)
                }
            }
        }

        is ChatItem.Plan -> Card(Modifier.fillMaxWidth()) {
            Column(Modifier.padding(12.dp)) {
                item.items.forEach { (done, content) ->
                    Text("${if (done) "☑" else "☐"} $content")
                }
            }
        }

        is ChatItem.AgentText -> Surface(
            shape = MaterialTheme.shapes.medium,
            color = MaterialTheme.colorScheme.surfaceVariant,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(item.text + if (item.streaming) " ▍" else "", Modifier.padding(12.dp))
        }

        is ChatItem.StatusLine -> Text(
            item.text,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** Full-screen dialog for reverse-RPC questions (§7). */
@Composable
private fun QuestionDialog(question: PendingQuestion, onAnswer: (String) -> Unit) {
    androidx.compose.ui.window.Dialog(onDismissRequest = { onAnswer("reject_once") }) {
        Surface(
            shape = MaterialTheme.shapes.large,
            tonalElevation = 6.dp,
        ) {
            Column(Modifier.padding(24.dp)) {
                Text(question.title, style = MaterialTheme.typography.headlineSmall)
                Spacer(Modifier.height(24.dp))
                question.options.forEach { option ->
                    Button(
                        onClick = { onAnswer(option) },
                        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                    ) {
                        Text(option.replace('_', ' '))
                    }
                }
            }
        }
    }
}
