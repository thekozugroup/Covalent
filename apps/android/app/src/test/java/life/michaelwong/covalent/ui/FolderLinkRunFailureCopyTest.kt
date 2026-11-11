package life.michaelwong.covalent.ui

import java.io.File
import life.michaelwong.covalent.R
import life.michaelwong.covalent.model.FolderLinkRunDestinationSummary
import life.michaelwong.covalent.model.FolderLinkRunFailure
import life.michaelwong.covalent.model.FolderLinkRunFailureRole
import life.michaelwong.covalent.model.FolderLinkRunPhase
import life.michaelwong.covalent.model.FolderLinkRunResult
import life.michaelwong.covalent.model.FolderLinkRunSummary
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderSharePhase
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A run's cause is shown only when it explains what the run on screen shows. Every other case
 * must leave the plain result exactly as it was before causes existed.
 */
class FolderLinkRunFailureCopyTest {

    @Test
    fun aRecipientSeesWhyItsOwnCopyFailed() {
        val share = share(incoming = true, generation = 7, destinationResult = FolderLinkRunResult.FAILED)
        val failure = failure(FolderLinkRunFailureRole.DESTINATION, generation = 7)
        assertEquals(failure, listOf(failure).causeFor(share))
        assertTrue(share.linkRun.hasLocalFailure())
    }

    @Test
    fun aSourceSeesWhyItCouldNotPrepareTheRun() {
        val share = share(incoming = false, generation = 3, phase = FolderLinkRunPhase.INTERRUPTED)
        val failure = failure(FolderLinkRunFailureRole.SOURCE, generation = 3)
        assertEquals(failure, listOf(failure).causeFor(share))
        assertTrue(share.linkRun.hasLocalFailure())
    }

    @Test
    fun anExplanationThatDoesNotMatchTheRunOnScreenIsNotShown() {
        val failed = share(incoming = true, generation = 7, destinationResult = FolderLinkRunResult.FAILED)
        // About an earlier run.
        assertNull(listOf(failure(FolderLinkRunFailureRole.DESTINATION, generation = 6)).causeFor(failed))
        // About another link.
        assertNull(
            listOf(failure(FolderLinkRunFailureRole.DESTINATION, generation = 7, folderId = OTHER_FOLDER))
                .causeFor(failed),
        )
        // The source never learns why a remote recipient failed, so a source-role entry cannot
        // explain a recipient's row, and a recipient-role entry cannot explain a source.
        assertNull(listOf(failure(FolderLinkRunFailureRole.SOURCE, generation = 7)).causeFor(failed))
        val outgoingFailed = share(incoming = false, generation = 7, destinationResult = FolderLinkRunResult.FAILED)
        assertNull(listOf(failure(FolderLinkRunFailureRole.DESTINATION, generation = 7)).causeFor(outgoingFailed))
        // The run on screen did not fail.
        val succeeded = share(incoming = true, generation = 7, destinationResult = FolderLinkRunResult.SUCCEEDED)
        assertNull(listOf(failure(FolderLinkRunFailureRole.DESTINATION, generation = 7)).causeFor(succeeded))
        assertFalse(succeeded.linkRun.hasLocalFailure())
        // No run at all, and no data at all.
        assertNull(listOf(failure(FolderLinkRunFailureRole.DESTINATION, generation = 7)).causeFor(failed.copy(linkRun = null)))
        assertNull(emptyList<FolderLinkRunFailure>().causeFor(failed))
        assertFalse(null.hasLocalFailure())
    }

    @Test
    fun everyDocumentedCauseHasItsOwnCopyAndAnythingElseReadsAsUnknown() {
        val documented = listOf(
            "noSpace", "permissionDenied", "readOnly", "integrityMismatch", "notFound", "network",
            "timeout", "sourceChanged", "folderUnavailable", "recoveryRequired", "workerUnavailable",
        )
        val resources = documented.map(::runFailureCauseRes)
        assertEquals("Two causes must never share one sentence", documented.size, resources.distinct().size)
        assertFalse(R.string.folder_link_run_cause_unknown in resources)
        assertEquals(R.string.folder_link_run_cause_unknown, runFailureCauseRes("unknown"))
        assertEquals(R.string.folder_link_run_cause_unknown, runFailureCauseRes("aCodeFromANewerNode"))
        assertEquals(R.string.folder_link_run_cause_unknown, runFailureCauseRes(""))
    }

    /** The node's own sentences say "share"; the app's copy follows the glossary instead. */
    @Test
    fun causeCopyUsesTheGlossaryAndSaysWhatToDo() {
        val xml = moduleFile("src/main/res/values/strings.xml").readText()
        val causes = Regex("<string name=\"(folder_link_run_cause_[a-z_]+)\">(.*?)</string>", RegexOption.DOT_MATCHES_ALL)
            .findAll(xml)
            .associate { it.groupValues[1] to it.groupValues[2] }
        assertEquals(12, causes.size)
        causes.forEach { (name, text) ->
            assertFalse("$name must say link, not share: $text", Regex("\\bshares?\\b", RegexOption.IGNORE_CASE).containsMatchIn(text))
            assertFalse("$name must not mention device logs: $text", text.contains("COVALENT_SYNC_DIAGNOSTIC"))
            assertTrue("$name must be finished sentences: $text", text.endsWith("."))
        }
    }

    private fun share(
        incoming: Boolean,
        generation: Long,
        phase: FolderLinkRunPhase? = FolderLinkRunPhase.SUCCEEDED,
        destinationResult: FolderLinkRunResult = FolderLinkRunResult.SUCCEEDED,
    ) = FolderShare(
        offerId = OFFER,
        folderId = FOLDER,
        label = "Camera",
        peerId = PEER,
        incoming = incoming,
        phase = FolderSharePhase.READY,
        expiresAtUnixMs = null,
        expired = false,
        linkRun = FolderLinkRunSummary(
            generation = generation,
            stateRevision = 1,
            settingsRevision = 1,
            phase = phase,
            startedAtUnixMs = 1,
            deadlineUnixMs = null,
            endedAtUnixMs = 2,
            nextDueAtUnixMs = null,
            pendingRequest = null,
            rejectedRequest = null,
            destinations = listOf(FolderLinkRunDestinationSummary(PEER, destinationResult, 2)),
        ),
    )

    private fun failure(role: FolderLinkRunFailureRole, generation: Long, folderId: String = FOLDER) =
        FolderLinkRunFailure(folderId, generation, role, "noSpace")

    private companion object {
        const val FOLDER = "11111111-1111-4111-8111-111111111111"
        const val OTHER_FOLDER = "44444444-4444-4444-8444-444444444444"
        const val PEER = "22222222-2222-4222-8222-222222222222"
        const val OFFER = "33333333-3333-4333-8333-333333333333"

        fun moduleFile(relative: String): File {
            val direct = File(relative)
            if (direct.isFile) return direct
            var candidate: File? = File("").absoluteFile
            while (candidate != null) {
                val resolved = File(candidate, "apps/android/app/$relative")
                if (resolved.isFile) return resolved
                candidate = candidate.parentFile
            }
            error("Unable to locate $relative from ${File("").absolutePath}")
        }
    }
}
