package ar.numis.papin.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.MenuAnchorType
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import ar.numis.papin.core.model.AgentDto
import ar.numis.papin.core.model.CatalogDto
import ar.numis.papin.core.model.stateLabel
import ar.numis.papin.di.AppContainer
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

class AgentsViewModel(private val container: AppContainer) : ViewModel() {
    private val _agents = MutableStateFlow<List<AgentDto>>(emptyList())
    val agents: StateFlow<List<AgentDto>> = _agents

    private val _catalog = MutableStateFlow<CatalogDto?>(null)
    val catalog: StateFlow<CatalogDto?> = _catalog

    private val _loading = MutableStateFlow(false)
    val loading: StateFlow<Boolean> = _loading

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error

    init {
        refresh()
    }

    fun refresh() {
        val api = container.gatewayApi ?: run {
            _error.value = "not onboarded"
            return
        }
        viewModelScope.launch {
            _loading.value = true
            runCatching { api.listAgents() }
                .onSuccess { _agents.value = it; _error.value = null }
                .onFailure { _error.value = it.message }
            _loading.value = false
        }
        viewModelScope.launch {
            if (_catalog.value == null) {
                runCatching { api.configCatalog() }.onSuccess { _catalog.value = it }
            }
        }
    }

    fun createAgent(name: String, base: String, seed: String?, onCreated: (AgentDto) -> Unit) {
        val api = container.gatewayApi ?: return
        viewModelScope.launch {
            runCatching { api.createAgent(name, base, seed) }
                .onSuccess { created ->
                    _agents.value = _agents.value + created
                    container.settings.defaultAgentId = created.id
                    onCreated(created)
                }
                .onFailure { _error.value = it.message }
        }
    }

    fun deleteAgent(id: String) {
        val api = container.gatewayApi ?: return
        viewModelScope.launch {
            runCatching { api.deleteAgent(id) }
                .onSuccess { _agents.value = _agents.value.filterNot { it.id == id } }
                .onFailure { _error.value = it.message }
        }
    }
}

/** Agent list home screen: state badges, pull-to-refresh, catalog-driven create. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AgentsScreen(container: AppContainer, onOpenChat: (String) -> Unit) {
    val model: AgentsViewModel = viewModel(factory = viewModelFactory { AgentsViewModel(container) })
    val agents by model.agents.collectAsState()
    val loading by model.loading.collectAsState()
    val error by model.error.collectAsState()
    val catalog by model.catalog.collectAsState()
    var showCreate by remember { mutableStateOf(false) }

    Scaffold(
        topBar = { TopAppBar(title = { Text("Agents") }) },
        floatingActionButton = {
            FloatingActionButton(onClick = { showCreate = true }) {
                Text("+")
            }
        },
    ) { padding ->
        PullToRefreshBox(
            isRefreshing = loading,
            onRefresh = { model.refresh() },
            modifier = Modifier.fillMaxSize().padding(padding),
        ) {
            when {
                agents.isEmpty() && !loading -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Text("No agents yet.", style = MaterialTheme.typography.bodyLarge)
                        Button(onClick = { showCreate = true }) { Text("Create one") }
                        error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                    }
                }
                else -> LazyColumn(
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    items(agents, key = { it.id }) { agent ->
                        AgentRow(agent, onClick = {
                            container.settings.defaultAgentId = agent.id
                            onOpenChat(agent.id)
                        }, onDelete = { model.deleteAgent(agent.id) })
                    }
                    error?.let {
                        item { Text(it, color = MaterialTheme.colorScheme.error) }
                    }
                }
            }
        }
    }

    if (showCreate) {
        CreateAgentDialog(
            catalog = catalog,
            onDismiss = { showCreate = false },
            onCreate = { name, base, seed ->
                showCreate = false
                model.createAgent(name, base, seed) { onOpenChat(it.id) } // create auto-connects
            },
        )
    }
}

@Composable
private fun AgentRow(agent: AgentDto, onClick: () -> Unit, onDelete: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(agent.stateLabel(), color = when (agent.state) {
            "active" -> MaterialTheme.colorScheme.primary
            "error" -> MaterialTheme.colorScheme.error
            else -> MaterialTheme.colorScheme.onSurfaceVariant
        })
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(agent.name.ifEmpty { agent.id }, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            Text(agent.id, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        TextButton(onClick = onDelete) { Text("Remove") }
    }
}

/** Create dialog driven by GET /api/v1/config-catalog (§7). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CreateAgentDialog(
    catalog: CatalogDto?,
    onDismiss: () -> Unit,
    onCreate: (name: String, base: String, seed: String?) -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var base by remember { mutableStateOf(catalog?.bases?.firstOrNull()?.name.orEmpty()) }
    var seed by remember { mutableStateOf<String?>(null) }
    var baseMenu by remember { mutableStateOf(false) }
    var seedMenu by remember { mutableStateOf(false) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("New agent") },
        text = {
            Column {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text("Name") },
                    modifier = Modifier.fillMaxWidth(),
                )
                if (catalog == null) {
                    CircularProgressIndicator(Modifier.padding(top = 16.dp))
                } else {
                    ExposedDropdownMenuBox(
                        expanded = baseMenu,
                        onExpandedChange = { baseMenu = it },
                        modifier = Modifier.padding(top = 8.dp),
                    ) {
                        OutlinedTextField(
                            value = base,
                            onValueChange = {},
                            readOnly = true,
                            label = { Text("Base image") },
                            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(baseMenu) },
                            modifier = Modifier.menuAnchor(MenuAnchorType.PrimaryNotEditable).fillMaxWidth(),
                        )
                        ExposedDropdownMenu(expanded = baseMenu, onDismissRequest = { baseMenu = false }) {
                            catalog.bases.forEach { item ->
                                DropdownMenuItem(
                                    text = { Text("${item.name} — ${item.description}") },
                                    onClick = { base = item.name; baseMenu = false },
                                )
                            }
                        }
                    }
                    ExposedDropdownMenuBox(
                        expanded = seedMenu,
                        onExpandedChange = { seedMenu = it },
                        modifier = Modifier.padding(top = 8.dp),
                    ) {
                        OutlinedTextField(
                            value = seed ?: "(no workspace template)",
                            onValueChange = {},
                            readOnly = true,
                            label = { Text("Workspace template") },
                            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(seedMenu) },
                            modifier = Modifier.menuAnchor(MenuAnchorType.PrimaryNotEditable).fillMaxWidth(),
                        )
                        ExposedDropdownMenu(expanded = seedMenu, onDismissRequest = { seedMenu = false }) {
                            DropdownMenuItem(
                                text = { Text("(none)") },
                                onClick = { seed = null; seedMenu = false },
                            )
                            catalog.seeds.forEach { item ->
                                DropdownMenuItem(
                                    text = { Text("${item.name} — ${item.description}") },
                                    onClick = { seed = item.name; seedMenu = false },
                                )
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            Button(
                onClick = { onCreate(name.trim().ifEmpty { "agent" }, base, seed) },
                enabled = base.isNotEmpty() && catalog != null,
            ) { Text("Create & connect") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
