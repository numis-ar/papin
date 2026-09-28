package ar.numis.papin.core.prefs

import android.content.Context
import ar.numis.papin.core.model.ProvisioningLink

/** On-device settings (gateway URL, device token, tunnel state, default agent). */
class SettingsStore(context: Context) {
    private val prefs = context.getSharedPreferences("papin", Context.MODE_PRIVATE)

    var gatewayUrl: String?
        get() = prefs.getString(KEY_URL, null)
        set(v) = prefs.edit().putString(KEY_URL, v).apply()

    var deviceToken: String?
        get() = prefs.getString(KEY_TOKEN, null)
        set(v) = prefs.edit().putString(KEY_TOKEN, v).apply()

    var defaultAgentId: String?
        get() = prefs.getString(KEY_AGENT, null)
        set(v) = prefs.edit().putString(KEY_AGENT, v).apply()

    var tunnelName: String?
        get() = prefs.getString(KEY_TUNNEL, null)
        set(v) = prefs.edit().putString(KEY_TUNNEL, v).apply()

    /** wg-quick config text imported at onboarding (reconnect source). */
    var tunnelConfig: String?
        get() = prefs.getString(KEY_TUNNEL_CONFIG, null)
        set(v) = prefs.edit().putString(KEY_TUNNEL_CONFIG, v).apply()

    var enrolled: Boolean
        get() = prefs.getBoolean(KEY_ENROLLED, false)
        set(v) = prefs.edit().putBoolean(KEY_ENROLLED, v).apply()

    var provisioningLink: ProvisioningLink?
        get() = prefs.getString(KEY_LINK, null)?.let { ProvisioningLink.parse(it) }
        set(v) = prefs.edit().putString(KEY_LINK, v?.toUriString()).apply()

    fun onboarded(): Boolean = enrolled && gatewayUrl != null && deviceToken != null

    /** Logout/clear (Settings screen). */
    fun clear() = prefs.edit().clear().apply()

    private companion object {
        const val KEY_URL = "gateway_url"
        const val KEY_TOKEN = "device_token"
        const val KEY_AGENT = "default_agent"
        const val KEY_TUNNEL = "tunnel_name"
        const val KEY_ENROLLED = "enrolled"
        const val KEY_TUNNEL_CONFIG = "tunnel_config"
        const val KEY_LINK = "provisioning_link"
    }
}
