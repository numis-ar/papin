package ar.numis.papin.ui.screens

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.lifecycle.viewmodel.compose.viewModel
import ar.numis.papin.core.model.EnrollFlow
import ar.numis.papin.core.model.EnrollState
import ar.numis.papin.core.model.ProvisioningLink
import ar.numis.papin.core.model.buildWgQuickConfig
import ar.numis.papin.di.AppContainer
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

class OnboardingViewModel(private val container: AppContainer) : ViewModel() {
    private val _state = MutableStateFlow<EnrollState>(EnrollState.Idle)
    val state: StateFlow<EnrollState> = _state

    fun start(link: ProvisioningLink, onDone: () -> Unit) {
        _state.value = EnrollState.GeneratingKeys
        val flow = EnrollFlow(
            generateKeypair = { container.tunnelManager.generateKeypair() },
            enroll = { _, pubkey ->
                container.gatewayApi(link.gatewayHttpUrl, link.token).enroll(pubkey)
            },
            buildTunnelConfig = { l, privateKey, response ->
                buildWgQuickConfig(l, privateKey, response)
            },
        )
        viewModelScope.launch {
            _state.value = EnrollState.Enrolling
            when (val result = flow.run(link)) {
                is EnrollState.Enrolled -> {
                    // Import + bring the tunnel up, then persist settings.
                    runCatching {
                        container.tunnelManager.connect("papin", result.tunnelConfig)
                    }.onSuccess {
                        container.settings.gatewayUrl = link.gatewayHttpUrl
                        container.settings.deviceToken = link.token
                        container.settings.tunnelName = "papin"
                        container.settings.provisioningLink = link
                        container.settings.tunnelConfig = result.tunnelConfig
                        container.settings.enrolled = true
                        onDone()
                    }.onFailure { e ->
                        _state.value = EnrollState.Failed("tunnel up failed: ${e.message}")
                    }
                }
                is EnrollState.Failed -> _state.value = result
                else -> Unit
            }
        }
    }

    fun parse(raw: String): ProvisioningLink? = ProvisioningLink.parse(raw)
}

/**
 * QR onboarding (§7.1): scan the provisioning QR (zxing) or paste the link;
 * the device generates its WireGuard keypair locally, enrolls with the
 * gateway, imports the tunnel config, and connects.
 */
@Composable
fun OnboardingScreen(container: AppContainer, onDone: () -> Unit) {
    val model: OnboardingViewModel = viewModel(
        factory = viewModelFactory { OnboardingViewModel(container) },
    )
    val state by model.state.collectAsState()
    var manual by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }

    val scanLauncher = rememberLauncherForActivityResult(ScanContract()) { result ->
        if (result.contents != null) {
            model.parse(result.contents)?.let { model.start(it, onDone) }
                ?: run { error = "not a Papin provisioning QR" }
        }
    }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text("Connect to your Papin gateway", style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(24.dp))
        when (state) {
            is EnrollState.GeneratingKeys, is EnrollState.Enrolling -> {
                CircularProgressIndicator()
                Spacer(Modifier.height(16.dp))
                Text("Generating keys on this device and enrolling…")
            }
            else -> {
                Button(onClick = {
                    val options = ScanOptions().setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                        .setPrompt("Scan the provisioning QR")
                        .setBeepEnabled(false)
                    scanLauncher.launch(options)
                }) {
                    Text("Scan provisioning QR")
                }
                Spacer(Modifier.height(16.dp))
                OutlinedTextField(
                    value = manual,
                    onValueChange = { manual = it },
                    modifier = Modifier.fillMaxWidth(),
                    label = { Text("or paste provisioning link") },
                )
                Spacer(Modifier.height(8.dp))
                Button(
                    onClick = {
                        model.parse(manual.trim())?.let { model.start(it, onDone) }
                            ?: run { error = "invalid provisioning link" }
                    },
                    enabled = manual.isNotBlank(),
                ) {
                    Text("Connect")
                }
            }
        }
        error?.let {
            Spacer(Modifier.height(16.dp))
            Text(it, color = MaterialTheme.colorScheme.error)
        }
        (state as? EnrollState.Failed)?.let {
            Spacer(Modifier.height(16.dp))
            Text(it.reason, color = MaterialTheme.colorScheme.error)
        }
    }
}
