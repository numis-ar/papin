package ar.numis.papin.di

import android.content.Context
import ar.numis.papin.core.acp.AcpClient
import ar.numis.papin.core.net.GatewayApi
import ar.numis.papin.core.prefs.SettingsStore
import ar.numis.papin.core.wg.TunnelManager
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import okhttp3.OkHttpClient
import java.util.concurrent.TimeUnit

/** Manual DI container (kept simple on purpose — no Hilt). */
class AppContainer(private val appContext: Context) {
    val appScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    val settings by lazy { SettingsStore(appContext) }

    val okHttpClient by lazy {
        OkHttpClient.Builder()
            .pingInterval(20, TimeUnit.SECONDS)
            .readTimeout(0, TimeUnit.MILLISECONDS) // WS is long-lived
            .build()
    }

    val gatewayApi: GatewayApi?
        get() {
            val url = settings.gatewayUrl ?: return null
            val token = settings.deviceToken ?: return null
            return GatewayApi(url, token, okHttpClient)
        }

    /** Explicit url+token variant (onboarding enrolls before settings persist). */
    fun gatewayApi(url: String, token: String): GatewayApi = GatewayApi(url, token, okHttpClient)

    val tunnelManager by lazy { TunnelManager(appContext) }

    fun acpClient(agentId: String): AcpClient? {
        val url = settings.gatewayUrl ?: return null
        val token = settings.deviceToken ?: return null
        return AcpClient(url, agentId, token, okHttpClient, appScope)
    }
}
