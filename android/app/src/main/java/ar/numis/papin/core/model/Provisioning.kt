package ar.numis.papin.core.model

import kotlinx.serialization.Serializable

/**
 * Provisioning link payload (PLAN §7.1). The QR carries everything the client
 * needs EXCEPT secrets — the WireGuard keypair is generated on the device:
 *
 *   papin://enroll?endpoint=<wg host:port>&server_pubkey=<b64>
 *     &gateway_ip=<tun ip>&gateway_port=<http port>
 *     &client_ip=<assigned tun ip>&token=<device token>
 */
@Serializable
data class ProvisioningLink(
    val endpoint: String,
    val serverPubkey: String,
    val gatewayIp: String,
    val gatewayPort: Int,
    val clientIp: String,
    val token: String,
) {
    val gatewayHttpUrl: String get() = "http://$gatewayIp:$gatewayPort"

    fun toUriString(): String =
        "papin://enroll?endpoint=$endpoint&server_pubkey=$serverPubkey" +
            "&gateway_ip=$gatewayIp&gateway_port=$gatewayPort" +
            "&client_ip=$clientIp&token=$token"

    companion object {
        fun parse(raw: String): ProvisioningLink? {
            val uri = runCatching { java.net.URI(raw) }.getOrNull() ?: return null
            if (uri.scheme != "papin" || uri.host != "enroll") return null
            val params = uri.rawQuery.orEmpty()
                .split('&')
                .filter { it.isNotEmpty() }
                .mapNotNull { pair ->
                    val (k, v) = pair.split('=', limit = 2).let {
                        it.getOrNull(0) to it.getOrNull(1)
                    }
                    if (k == null || v == null) null else k to java.net.URLDecoder.decode(v, Charsets.UTF_8)
                }
                .toMap()
            val endpoint = params["endpoint"] ?: return null
            val serverPubkey = params["server_pubkey"] ?: return null
            val gatewayIp = params["gateway_ip"] ?: return null
            val clientIp = params["client_ip"] ?: return null
            val token = params["token"] ?: return null
            val gatewayPort = params["gateway_port"]?.toIntOrNull() ?: 8080
            return ProvisioningLink(
                endpoint = endpoint,
                serverPubkey = serverPubkey,
                gatewayIp = gatewayIp,
                gatewayPort = gatewayPort,
                clientIp = clientIp,
                token = token,
            )
        }
    }
}
