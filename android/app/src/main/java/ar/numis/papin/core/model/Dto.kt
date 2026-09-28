package ar.numis.papin.core.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

/** `GET /api/v1/agents` item. */
@Serializable
data class AgentDto(
    val id: String,
    val name: String = "",
    @SerialName("created_at") val createdAt: Long = 0,
    val state: String = "stopped", // stopped | active | error
    val config: AgentConfigDto? = null,
)

@Serializable
data class AgentConfigDto(
    val base: String,
    val seed: String? = null,
)

/** `GET /api/v1/config-catalog`. */
@Serializable
data class CatalogDto(
    val bases: List<CatalogItemDto> = emptyList(),
    val seeds: List<CatalogItemDto> = emptyList(),
    val env: List<CatalogEnvDto> = emptyList(),
)

@Serializable
data class CatalogItemDto(
    val name: String,
    val description: String = "",
)

@Serializable
data class CatalogEnvDto(
    val key: String,
    val default: String = "",
)

/** `POST /api/v1/enroll` request/response. */
@Serializable
data class EnrollRequestDto(
    @SerialName("client_pubkey") val clientPubkey: String,
)

@Serializable
data class EnrollResponseDto(
    @SerialName("assigned_ip") val assignedIp: String,
    @SerialName("gateway_ip") val gatewayIp: String,
)

/** `session/list` response item. */
@Serializable
data class SessionInfoDto(
    @SerialName("sessionId") val sessionId: String,
    val title: String = "",
)

fun AgentDto.stateLabel(): String = when (state) {
    "active" -> "●"
    "error" -> "⚠"
    else -> "○"
}
