package ar.numis.papin.service

import android.net.VpnService

/**
 * Userspace WireGuard tunnel service. GoBackend (from
 * com.wireguard.android:tunnel) runs the wireguard-go userspace backend on
 * top of this VpnService's tun fd — no root required.
 *
 * GoBackend needs the VpnService context to establish the tun interface, so
 * the instance is published here for [ar.numis.papin.core.wg.TunnelManager].
 */
class PapinVpnService : VpnService() {
    override fun onCreate() {
        super.onCreate()
        instance = this
    }

    override fun onDestroy() {
        if (instance === this) instance = null
        super.onDestroy()
    }

    companion object {
        @Volatile var instance: PapinVpnService? = null
            private set
    }
}
