package ar.numis.papin.core.model

/**
 * QR onboarding flow (§7.1), as a pure state machine. The ViewModel supplies
 * the effects; tests supply fakes. Key generation stays on the device — the
 * QR/link never carries a private key.
 */
sealed interface EnrollState {
    data object Idle : EnrollState
    data object GeneratingKeys : EnrollState
    data object Enrolling : EnrollState
    data class Ready(val link: ProvisioningLink) : EnrollState
    data class Enrolled(val tunnelConfig: String) : EnrollState
    data class Failed(val reason: String) : EnrollState
}

class EnrollFlow(
    private val generateKeypair: suspend () -> Pair<String, String>, // (privateKeyB64, publicKeyB64)
    private val enroll: suspend (token: String, clientPubkey: String) -> EnrollResponseDto,
    private val buildTunnelConfig: (link: ProvisioningLink, privateKey: String, response: EnrollResponseDto) -> String,
) {
    suspend fun run(link: ProvisioningLink): EnrollState {
        val (privateKey, publicKey) = try {
            generateKeypair()
        } catch (e: Exception) {
            return EnrollState.Failed("key generation failed: ${e.message}")
        }
        val response = try {
            enroll(link.token, publicKey)
        } catch (e: Exception) {
            return EnrollState.Failed("enrollment failed: ${e.message}")
        }
        val config = runCatching { buildTunnelConfig(link, privateKey, response) }
            .getOrElse { e -> return EnrollState.Failed("invalid provisioning data: ${e.message}") }
        return EnrollState.Enrolled(config)
    }
}

/** wg-quick-style config the WireGuard tunnel is imported from. */
fun buildWgQuickConfig(
    link: ProvisioningLink,
    privateKey: String,
    response: EnrollResponseDto,
): String = buildString {
    appendLine("[Interface]")
    appendLine("PrivateKey = $privateKey")
    appendLine("Address = ${response.assignedIp}/32")
    appendLine("DNS = 1.1.1.1")
    appendLine()
    appendLine("[Peer]")
    appendLine("PublicKey = ${link.serverPubkey}")
    appendLine("Endpoint = ${link.endpoint}")
    appendLine("AllowedIPs = 0.0.0.0/0, ::/0")
    appendLine("PersistentKeepalive = 25")
}
