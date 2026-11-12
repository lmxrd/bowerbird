package eu.vardalabs.organizer.core.api

import eu.vardalabs.organizer.api.models.Choice
import eu.vardalabs.organizer.api.models.MoveFile
import org.junit.Assert.assertEquals
import org.junit.Test

class GeneratedJsonTest {
    @Test
    fun `nulls are left out of request bodies`() {
        // The server refuses `"new_folders": null` (it expects a list or nothing).
        assertEquals("{}", GeneratedJson.json.encodeToString(Choice.serializer(), Choice()))
        assertEquals(
            """{"if_version":3,"name":"b.pdf"}""",
            GeneratedJson.json.encodeToString(MoveFile.serializer(), MoveFile(ifVersion = 3, name = "b.pdf")),
        )
    }
}
