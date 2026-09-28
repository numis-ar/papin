package ar.numis.papin.core.model

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class EnrollFlowTest {
    private val link = ProvisioningLink(
        endpoint = "vpn.example.com:51820",
        serverPubkey = "SERVERPUBKEY",
        gatewayIp = "10.77.0.1",
        gatewayPort = 8080,
        clientIp = "10.77.0.2",
        token = "tok",
    )

    @Test
    fun happyPathGeneratesKeysEnrollsAndBuildsConfig() = runTest {
        var enrolledPubkey: String? = null
        var enrolledToken: String? = null
        val flow = EnrollFlow(
            generateKeypair = { "PRIVKEY" to "PUBKEY" },
            enroll = { token, pubkey ->
                enrolledToken = token
                enrolledPubkey = pubkey
                EnrollResponseDto(assignedIp = "10.77.0.2", gatewayIp = "10.77.0.1")
            },
            buildTunnelConfig = { l, privateKey, response ->
                buildWgQuickConfig(l, privateKey, response)
            },
        )
        val result = flow.run(link)
        assertTrue(result is EnrollState.Enrolled)
        val config = (result as EnrollState.Enrolled).tunnelConfig
        assertTrue(config.contains("PrivateKey = PRIVKEY"))
        assertTrue(config.contains("PublicKey = SERVERPUBKEY"))
        assertTrue(config.contains("Address = 10.77.0.2/32"))
        assertTrue(config.contains("Endpoint = vpn.example.com:51820"))
        assertEquals("tok", enrolledToken)
        assertEquals("PUBKEY", enrolledPubkey)
        // The QR never carries the device private key.
        assertTrue(!link.toUriString().contains("PRIVKEY"))
    }

    @Test
    fun enrollFailureIsReported() = runTest {
        val flow = EnrollFlow(
            generateKeypair = { "PRIV" to "PUB" },
            enroll = { _, _ -> error("HTTP 401: device token unknown") },
            buildTunnelConfig = { l, k, r -> buildWgQuickConfig(l, k, r) },
        )
        val result = flow.run(link)
        assertTrue(result is EnrollState.Failed)
        assertTrue((result as EnrollState.Failed).reason.contains("401"))
    }

    @Test
    fun keyGenerationFailureIsReported() = runTest {
        val flow = EnrollFlow(
            generateKeypair = { error("no secure random") },
            enroll = { _, _ -> EnrollResponseDto("10.0.0.2", "10.0.0.1") },
            buildTunnelConfig = { l, k, r -> buildWgQuickConfig(l, k, r) },
        )
        assertTrue(flow.run(link) is EnrollState.Failed)
    }
}
