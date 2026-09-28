package ar.numis.papin.core.net

import ar.numis.papin.core.model.AgentDto
import ar.numis.papin.core.model.CatalogDto
import ar.numis.papin.core.model.EnrollRequestDto
import ar.numis.papin.core.model.EnrollResponseDto
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody

/** Gateway REST client (the /api/v1 family). */
class GatewayApi(
    private val baseUrl: String, // e.g. http://10.77.0.1:8080
    private val token: String,
    private val client: OkHttpClient,
    private val json: Json = Json { ignoreUnknownKeys = true },
) {
    private val base = baseUrl.trimEnd('/')

    suspend fun healthz(): Boolean = withContext(Dispatchers.IO) {
        runCatching {
            client.newCall(Request.Builder().url("$base/healthz").build()).execute()
                .use { it.isSuccessful }
        }.getOrDefault(false)
    }

    suspend fun listAgents(): List<AgentDto> = get("/api/v1/agents", ListSerializer(AgentDto.serializer()))

    suspend fun configCatalog(): CatalogDto = get("/api/v1/config-catalog", CatalogDto.serializer())

    suspend fun createAgent(name: String, base: String, seed: String?): AgentDto =
        post(
            "/api/v1/agents",
            buildJsonObject {
                put("name", name)
                putJsonObject("config") {
                    put("base", base)
                    if (seed != null) put("seed", seed)
                }
            },
            AgentDto.serializer(),
        )

    suspend fun deleteAgent(id: String, force: Boolean = false) {
        val path = "/api/v1/agents/$id" + if (force) "?force=true" else ""
        withContext(Dispatchers.IO) {
            client.newCall(
                Request.Builder().url("$base$path").header("Authorization", bearer()).delete().build(),
            ).execute().use { require(it.isSuccessful) { "delete failed: ${it.code}" } }
        }
    }

    suspend fun enroll(clientPubkey: String): EnrollResponseDto =
        post("/api/v1/enroll", EnrollRequestDto.serializer(), EnrollRequestDto(clientPubkey), EnrollResponseDto.serializer())

    private fun bearer() = "Bearer $token"

    private suspend fun <T> get(path: String, serializer: kotlinx.serialization.KSerializer<T>): T =
        withContext(Dispatchers.IO) {
            client.newCall(
                Request.Builder().url("$base$path").header("Authorization", bearer()).build(),
            ).execute().use { resp ->
                require(resp.isSuccessful) { "GET $path failed: ${resp.code}" }
                json.decodeFromString(serializer, resp.body!!.string())
            }
        }

    private suspend fun <T> post(
        path: String,
        body: JsonObject,
        serializer: kotlinx.serialization.KSerializer<T>,
    ): T = withContext(Dispatchers.IO) {
        val bodyText = json.encodeToString(JsonObject.serializer(), body)
        client.newCall(
            Request.Builder()
                .url("$base$path")
                .header("Authorization", bearer())
                .post(bodyText.toRequestBody(JSON_MEDIA))
                .build(),
        ).execute().use { resp ->
            require(resp.isSuccessful) { "POST $path failed: ${resp.code} ${resp.body?.string()}" }
            json.decodeFromString(serializer, resp.body!!.string())
        }
    }

    private suspend fun <Q, T> post(
        path: String,
        requestSerializer: kotlinx.serialization.KSerializer<Q>,
        request: Q,
        responseSerializer: kotlinx.serialization.KSerializer<T>,
    ): T = withContext(Dispatchers.IO) {
        val bodyText = json.encodeToString(requestSerializer, request)
        client.newCall(
            Request.Builder()
                .url("$base$path")
                .header("Authorization", bearer())
                .post(bodyText.toRequestBody(JSON_MEDIA))
                .build(),
        ).execute().use { resp ->
            require(resp.isSuccessful) { "POST $path failed: ${resp.code} ${resp.body?.string()}" }
            json.decodeFromString(responseSerializer, resp.body!!.string())
        }
    }

    private companion object {
        val JSON_MEDIA = "application/json; charset=utf-8".toMediaType()
    }
}
