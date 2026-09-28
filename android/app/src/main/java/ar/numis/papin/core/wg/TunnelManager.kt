package ar.numis.papin.core.wg

import android.content.Context
import android.content.Intent
import android.net.VpnService
import ar.numis.papin.service.PapinVpnService
import com.wireguard.android.backend.Backend
import com.wireguard.android.backend.State
import com.wireguard.android.backend.Tunnel
import com.wireguard.config.Config
import com.wireguard.crypto.KeyPair
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.StringReader

/**
 * Userspace WireGuard tunnel manager (PLAN §7) built on the official
 * `com.wireguard.android:tunnel` GoBackend. The keypair is generated on the
 * device; nothing secret ever leaves it.
 */
class TunnelManager(
    private val context: Context,
) {
    @Volatile private var backend: Backend? = null
    private var tunnel: Tunnel? = null

    /** Generate a fresh WireGuard keypair (device-local). */
    fun generateKeypair(): Pair<String, String> {
        val pair = KeyPair()
        return pair.privateKey.base64() to pair.publicKey.base64()
    }

    /** Parse + import + bring the tunnel up. Idempotent per name. */
    suspend fun connect(name: String, wgQuick: String) = withContext(Dispatchers.IO) {
        val parsed = Config.parse(StringReader(wgQuick))
        // GoBackend must run on the VpnService context (it owns the tun fd).
        val service = ensureVpnService()
        val b = backendFor(service)
        val t = object : Tunnel {
            override fun getName() = name
            override fun getConfig() = parsed
            override fun onStateChange(newState: State) = Unit
        }
        tunnel = t
        b.setState(t, State.UP, parsed)
        Unit
    }

    /** Start [ar.numis.papin.service.PapinVpnService] and wait for its instance. */
    private fun ensureVpnService(): android.net.VpnService {
        PapinVpnService.instance?.let { return it }
        context.startService(Intent(context, PapinVpnService::class.java))
        val deadline = System.nanoTime() + 5_000_000_000L
        while (System.nanoTime() < deadline) {
            PapinVpnService.instance?.let { return it }
            Thread.sleep(50)
        }
        error("VpnService did not start")
    }

    suspend fun disconnect() = withContext(Dispatchers.IO) {
        tunnel?.let { backend?.setState(it, State.DOWN, null) }
        tunnel = null
    }

    fun isUp(): Boolean = tunnel?.let { backend?.getState(it) == State.UP } ?: false

    private fun backendFor(context: Context): Backend =
        backend ?: synchronized(this) {
            backend ?: com.wireguard.android.backend.GoBackend(context.applicationContext).also { backend = it }
        }
}
