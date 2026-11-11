package eu.vardalabs.organizer.core.api

import eu.vardalabs.organizer.api.infrastructure.Serializer
import kotlinx.serialization.json.Json

/**
 * The generated client's JSON, set up once before it is first used: nulls
 * are left out of request bodies. The server reads an absent field as its
 * default, but refuses `null` where it expects a list (`Choice.new_folders`).
 */
object GeneratedJson {
    init {
        Serializer.kotlinxSerializationJsonConfiguration = { explicitNulls = false }
    }

    val json: Json get() = Serializer.kotlinxSerializationJson

    /** Make sure [init] ran: called before the generated client is used. */
    fun ensure() = Unit
}
