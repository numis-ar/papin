package ar.numis.papin.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import ar.numis.papin.di.AppContainer
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

class SettingsViewModel(private val container: AppContainer) : ViewModel() {
    private val _url = MutableStateFlow(container.settings.gatewayUrl ?: "")
    val url: StateFlow<String> = _url

    private val _token = MutableStateFlow(container.settings.deviceToken ?: "")
    val token: StateFlow<String> = _token

    private val _tunnelUp = MutableStateFlow(container.tunnelManager.isUp())
    val tunnelUp: StateFlow<Boolean> = _tunnelUp

    private val _health = MutableStateFlow<Boolean?>(null)
    val health: StateFlow<Boolean?> = _health

    fun onUrl(v: String) {
        _url.value = v
        container.settings.gatewayUrl = v.ifEmpty { null }
    }

    fun onToken(v: String) {
        _token.value = v
        container.settings.deviceToken = v.ifEmpty { null }
    }

    fun checkHealth() {
        val api = container.gatewayApi ?: run { _health.value = false; return }
        viewModelScope.launch {
            _health.value = runCatching { api.healthz() }.getOrDefault(false)
        }
    }

    fun reconnectTunnel() {
        val config = container.settings.tunnelConfig ?: return
        viewModelScope.launch {
            container.tunnelManager.disconnect()
            runCatching { container.tunnelManager.connect("papin", config) }
            _tunnelUp.value = container.tunnelManager.isUp()
        }
    }

    /** Logout/clear: wipe settings; next launch returns to onboarding. */
    fun logout() {
        container.settings.clear()
    }
}

/** Settings: gateway URL, device token, tunnel status, health check, logout. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(container: AppContainer) {
    val model: SettingsViewModel = viewModel(factory = viewModelFactory { SettingsViewModel(container) })
    val url by model.url.collectAsState()
    val token by model.token.collectAsState()
    val tunnelUp by model.tunnelUp.collectAsState()
    val health by model.health.collectAsState()
    var confirmLogout by remember { mutableStateOf(false) }

    Scaffold(topBar = { TopAppBar(title = { Text("Settings") }) }) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Spacer(Modifier.height(4.dp))
            OutlinedTextField(
                value = url,
                onValueChange = model::onUrl,
                label = { Text("Gateway URL") },
                modifier = Modifier.fillMaxWidth(),
                singleLine = true,
            )
            OutlinedTextField(
                value = token,
                onValueChange = model::onToken,
                label = { Text("Device token") },
                modifier = Modifier.fillMaxWidth(),
                singleLine = true,
            )
            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp)) {
                    Text("Tunnel", style = MaterialTheme.typography.titleSmall)
                    Text(if (tunnelUp) "WireGuard tunnel: up" else "WireGuard tunnel: down")
                    Spacer(Modifier.height(8.dp))
                    Button(onClick = model::reconnectTunnel) { Text("Reconnect tunnel") }
                }
            }
            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(16.dp)) {
                    Text("Gateway", style = MaterialTheme.typography.titleSmall)
                    when (health) {
                        true -> Text("Reachable ✓", color = MaterialTheme.colorScheme.primary)
                        false -> Text("Unreachable — check the tunnel", color = MaterialTheme.colorScheme.error)
                        null -> Text("Not checked yet")
                    }
                    Spacer(Modifier.height(8.dp))
                    Button(onClick = model::checkHealth) { Text("Check health") }
                }
            }
            Spacer(Modifier.height(8.dp))
            Button(onClick = { confirmLogout = true }, modifier = Modifier.fillMaxWidth()) {
                Text("Log out / clear this device")
            }
        }
    }

    if (confirmLogout) {
        androidx.compose.material3.AlertDialog(
            onDismissRequest = { confirmLogout = false },
            title = { Text("Log out?") },
            text = { Text("Clears the gateway URL, device token, and enrollment on this device. The WireGuard tunnel stays up until disconnected.") },
            confirmButton = {
                Button(onClick = { model.logout(); confirmLogout = false }) { Text("Log out") }
            },
            dismissButton = {
                androidx.compose.material3.TextButton(onClick = { confirmLogout = false }) { Text("Cancel") }
            },
        )
    }
}
