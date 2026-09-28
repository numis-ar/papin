package ar.numis.papin.core.model

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ProvisioningLinkTest {
    @Test
    fun roundTrip() {
        val link = ProvisioningLink(
            endpoint = "vpn.example.com:51820",
            serverPubkey = "dGVzdA==",
            gatewayIp = "10.77.0.1",
            gatewayPort = 8080,
            clientIp = "10.77.0.2",
            token = "abc123",
        )
        val parsed = ProvisioningLink.parse(link.toUriString())
        assertEquals(link, parsed)
        assertEquals("http://10.77.0.1:8080", parsed!!.gatewayHttpUrl)
    }

    @Test
    fun parsesEncodedQuery() {
        val parsed = ProvisioningLink.parse(
            "papin://enroll?endpoint=host%3A51820&server_pubkey=AAAA&gateway_ip=10.0.0.1&gateway_port=80&client_ip=10.0.0.2&token=t%20k",
        )
        assertEquals("host:51820", parsed!!.endpoint)
        assertEquals("t k", parsed.token)
        assertEquals(80, parsed.gatewayPort)
    }

    @Test
    fun rejectsNonPapinAndGarbage() {
        assertNull(ProvisioningLink.parse("https://example.com/x"))
        assertNull(ProvisioningLink.parse("kimi://other?x=1"))
        assertNull(ProvisioningLink.parse("not a uri at all :::"))
        assertNull(
            ProvisioningLink.parse(
                "papin://enroll?endpoint=e&server_pubkey=k", // missing fields
            ),
        )
    }

    @Test
    fun gatewayPortDefaultsTo8080() {
        val parsed = ProvisioningLink.parse(
            "papin://enroll?endpoint=e&server_pubkey=k&gateway_ip=10.0.0.1&client_ip=10.0.0.2&token=t",
        )
        assertEquals(8080, parsed!!.gatewayPort)
    }
}
