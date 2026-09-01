package life.michaelwong.covalent.data

import java.security.MessageDigest
import org.json.JSONObject

/** Minimal durable string storage used by the transfer crash-consistency journal. */
internal interface DurableTransferStorage {
    fun read(key: String): String?
    fun keys(): Set<String>
    fun commit(puts: Map<String, String>, removals: Set<String>): Boolean
}

/** Protects journal values without coupling the crash-state machine to Android Keystore APIs. */
internal interface TransferValueProtector {
    fun protect(value: String): String
    fun unprotect(value: String): String
}

internal class DurableTransferPersistenceException(message: String) : IllegalStateException(message)

/**
 * Crash-consistent state machine for requests whose terminal server result needs acknowledgement.
 *
 * Each method that permits a subsequent network side effect uses one synchronous storage commit.
 * The caller may therefore send only after the method returns. A failed commit throws and leaves
 * the preceding recoverable state authoritative.
 */
internal class DurableTransferJournal(
    private val storage: DurableTransferStorage,
    private val protector: TransferValueProtector,
) {
    @Synchronized
    fun savePending(
        jobId: String,
        path: String,
        payload: JSONObject,
        mode: String,
        treeUri: String?,
    ) {
        requireNewJob(jobId)
        val request = pendingEnvelope(jobId, path, payload, mode, treeUri)
        validatePending(jobId, request)
        commitOrThrow(
            puts = mapOf(pendingKey(jobId) to protector.protect(request.toString())),
            message = "The transfer request could not be saved durably.",
        )
    }

    @Synchronized
    fun saveQueued(
        jobId: String,
        path: String,
        payload: JSONObject,
        mode: String,
        treeUri: String?,
        transferRecord: JSONObject,
    ) {
        requireNewJob(jobId)
        val request = pendingEnvelope(jobId, path, payload, mode, treeUri)
        validatePending(jobId, request)
        validateTerminalRecord(jobId, transferRecord, expectedState = "queued")
        commitOrThrow(
            puts = mapOf(
                pendingKey(jobId) to protector.protect(request.toString()),
                transferKey(jobId) to protector.protect(transferRecord.toString()),
            ),
            message = "The queued transfer could not be saved durably.",
        )
    }

    /** Re-commits the exact request before its first or resumed network attempt. */
    @Synchronized
    fun preparePending(jobId: String): JSONObject? {
        val persisted = storage.read(pendingKey(jobId)) ?: return null
        val request = JSONObject(protector.unprotect(persisted))
        val normalized = normalizeLegacyPending(jobId, request)
        validatePending(jobId, normalized)
        commitOrThrow(
            puts = mapOf(pendingKey(jobId) to protector.protect(normalized.toString())),
            message = "The transfer request could not be confirmed on durable storage.",
        )
        return JSONObject(normalized.toString())
    }

    fun pending(jobId: String): JSONObject? {
        val persisted = storage.read(pendingKey(jobId)) ?: return null
        return normalizeLegacyPending(jobId, JSONObject(protector.unprotect(persisted))).also {
            validatePending(jobId, it)
        }
    }

    fun pendingJobIds(): List<String> = storage.keys().asSequence()
        .filter { it.startsWith(PENDING_PREFIX) }
        .map { it.removePrefix(PENDING_PREFIX) }
        .filter(String::isNotBlank)
        .sorted()
        .toList()

    /**
     * Atomically consumes a request into a completed UI record and a durable pending ACK.
     * The server acknowledgement must not be sent until this transaction succeeds.
     */
    @Synchronized
    fun consumeTerminalResult(
        jobId: String,
        completionDetail: String,
        completedTransferRecord: JSONObject,
    ) {
        val request = pending(jobId)
            ?: throw DurableTransferPersistenceException("The completed transfer has no durable request.")
        validateTerminalRecord(jobId, completedTransferRecord, expectedState = "completed")
        val acknowledgement = JSONObject()
            .put("schemaVersion", SCHEMA_VERSION)
            .put("jobId", jobId)
            .put("requestDigest", sha256(request.toString()))
            .put("terminalRecordDigest", sha256(completedTransferRecord.toString()))
            .put("completionDetail", completionDetail)
        commitOrThrow(
            puts = mapOf(
                acknowledgementKey(jobId) to protector.protect(acknowledgement.toString()),
                transferKey(jobId) to protector.protect(completedTransferRecord.toString()),
            ),
            removals = setOf(pendingKey(jobId)),
            message = "The completed transfer could not be saved before acknowledgement.",
        )
    }

    @Synchronized
    fun consumeTerminalResultWithoutAcknowledgement(
        jobId: String,
        completedTransferRecord: JSONObject,
    ) {
        checkNotNull(pending(jobId)) { "The completed transfer has no durable request." }
        validateTerminalRecord(jobId, completedTransferRecord, expectedState = "completed")
        commitOrThrow(
            puts = mapOf(transferKey(jobId) to protector.protect(completedTransferRecord.toString())),
            removals = setOf(pendingKey(jobId)),
            message = "The completed transfer could not be saved durably.",
        )
    }

    fun pendingAcknowledgementJobIds(): List<String> = storage.keys().asSequence()
        .filter { it.startsWith(ACKNOWLEDGEMENT_PREFIX) }
        .map { it.removePrefix(ACKNOWLEDGEMENT_PREFIX) }
        .filter(String::isNotBlank)
        .sorted()
        .toList()

    /** Validates all job metadata before the caller is allowed to send an ACK. */
    fun acknowledgementCompletionDetail(jobId: String): String? {
        return validatedAcknowledgement(jobId)?.completionDetail
    }

    /** Re-commits the exact ACK and terminal record before the server ACK is sent. */
    @Synchronized
    fun prepareAcknowledgement(jobId: String): String? {
        val validated = validatedAcknowledgement(jobId) ?: return null
        commitOrThrow(
            puts = mapOf(
                acknowledgementKey(jobId) to protector.protect(validated.acknowledgementJson),
                transferKey(jobId) to protector.protect(validated.transferJson),
            ),
            message = "The pending acknowledgement could not be confirmed on durable storage.",
        )
        return validated.completionDetail
    }

    private fun validatedAcknowledgement(jobId: String): ValidatedAcknowledgement? {
        requireValidJobId(jobId)
        val stored = storage.read(acknowledgementKey(jobId)) ?: return null
        val acknowledgementJson = protector.unprotect(stored)
        val acknowledgement = JSONObject(acknowledgementJson)
        require(acknowledgement.optInt("schemaVersion") == SCHEMA_VERSION) {
            "The pending acknowledgement schema is unsupported."
        }
        require(acknowledgement.getString("jobId") == jobId) {
            "The pending acknowledgement belongs to a different job."
        }
        require(acknowledgement.getString("requestDigest").matches(LOWERCASE_SHA256)) {
            "The pending acknowledgement request binding is invalid."
        }
        val transferJson = storage.read(transferKey(jobId))
            ?.let(protector::unprotect)
            ?: error("The pending acknowledgement has no completed transfer record.")
        require(acknowledgement.getString("terminalRecordDigest") == sha256(transferJson)) {
            "The pending acknowledgement does not match its completed transfer record."
        }
        val transfer = JSONObject(transferJson)
        validateTerminalRecord(jobId, transfer, expectedState = "completed")
        return ValidatedAcknowledgement(
            acknowledgementJson = acknowledgementJson,
            transferJson = transferJson,
            completionDetail = acknowledgement.getString("completionDetail"),
        )
    }

    /** Removes a successfully sent ACK only in the same commit as its final UI state. */
    @Synchronized
    fun confirmAcknowledged(jobId: String, completedTransferRecord: JSONObject) {
        checkNotNull(acknowledgementCompletionDetail(jobId)) {
            "The acknowledgement has no durable pending state."
        }
        validateTerminalRecord(jobId, completedTransferRecord, expectedState = "completed")
        commitOrThrow(
            puts = mapOf(transferKey(jobId) to protector.protect(completedTransferRecord.toString())),
            removals = setOf(acknowledgementKey(jobId)),
            message = "The acknowledged transfer could not be finalized durably.",
        )
    }

    @Synchronized
    fun removePending(jobId: String) {
        commitOrThrow(
            removals = setOf(pendingKey(jobId)),
            message = "The pending transfer could not be removed durably.",
        )
    }

    @Synchronized
    fun removePendingAcknowledgement(jobId: String) {
        commitOrThrow(
            removals = setOf(acknowledgementKey(jobId)),
            message = "The pending acknowledgement could not be removed durably.",
        )
    }

    private fun normalizeLegacyPending(jobId: String, request: JSONObject): JSONObject =
        JSONObject(request.toString())
            .put("schemaVersion", SCHEMA_VERSION)
            .put("jobId", request.optString("jobId", jobId))

    private fun pendingEnvelope(
        jobId: String,
        path: String,
        payload: JSONObject,
        mode: String,
        treeUri: String?,
    ): JSONObject = JSONObject()
        .put("schemaVersion", SCHEMA_VERSION)
        .put("jobId", jobId)
        .put("path", path)
        .put("payload", JSONObject(payload.toString()))
        .put("mode", mode)
        .also { value -> treeUri?.let { value.put("treeUri", it) } }

    private fun validatePending(jobId: String, request: JSONObject) {
        requireValidJobId(jobId)
        require(request.optInt("schemaVersion") == SCHEMA_VERSION && request.getString("jobId") == jobId) {
            "The pending transfer belongs to a different job."
        }
        val path = request.getString("path")
        val mode = request.optString("mode", "json")
        val payload = request.getJSONObject("payload")
        when (mode) {
            MODE_SAF_BACKUP -> {
                require(path == "/api/v1/backups/archive" && payload.getString("jobId") == jobId) {
                    "The backup request metadata does not match its durable job."
                }
                require(request.getString("treeUri").isNotBlank()) { "The backup source is missing." }
            }
            MODE_SAF_RESTORE -> {
                val reference = payload.getJSONObject("planReference")
                val expectedPlanId = payload.optString("expectedPlanId").takeIf(String::isNotBlank)
                val referencePlanId = reference.optString("planId").takeIf(String::isNotBlank)
                val requestPlanId = payload.getJSONObject("restoreRequest")
                    .optString("planId")
                    .takeIf(String::isNotBlank)
                require(
                    path == "/api/v1/restores/archive/execute" &&
                        reference.getString("jobId") == jobId &&
                        expectedPlanId == referencePlanId &&
                        requestPlanId == referencePlanId &&
                        payload.getString("expectedPlanDigest") == reference.getString("planDigest"),
                ) { "The restore request metadata does not match its durable job and plan." }
                require(request.getString("treeUri").isNotBlank()) { "The restore target is missing." }
            }
            "json" -> payload.optString("jobId").takeIf(String::isNotBlank)?.let { boundJobId ->
                require(boundJobId == jobId) { "The request metadata belongs to a different job." }
            }
            else -> error("The pending transfer mode is unsupported.")
        }
    }

    private fun validateTerminalRecord(jobId: String, record: JSONObject, expectedState: String) {
        requireValidJobId(jobId)
        require(record.getString("jobId") == jobId && record.getString("state") == expectedState) {
            "The transfer record does not match its durable job state."
        }
    }

    private fun requireValidJobId(jobId: String) {
        require(jobId.matches(SAFE_JOB_ID)) { "The transfer job ID is invalid." }
    }

    private fun requireNewJob(jobId: String) {
        requireValidJobId(jobId)
        require(
            storage.read(pendingKey(jobId)) == null &&
                storage.read(acknowledgementKey(jobId)) == null &&
                storage.read(transferKey(jobId)) == null,
        ) { "The transfer job ID is already bound to durable metadata." }
    }

    private fun commitOrThrow(
        puts: Map<String, String> = emptyMap(),
        removals: Set<String> = emptySet(),
        message: String,
    ) {
        if (!storage.commit(puts, removals)) throw DurableTransferPersistenceException(message)
    }

    private fun sha256(value: String): String = MessageDigest.getInstance("SHA-256")
        .digest(value.encodeToByteArray())
        .joinToString("") { "%02x".format(it) }

    private fun pendingKey(jobId: String) = "$PENDING_PREFIX$jobId"
    private fun acknowledgementKey(jobId: String) = "$ACKNOWLEDGEMENT_PREFIX$jobId"
    private fun transferKey(jobId: String) = "$TRANSFER_PREFIX$jobId"

    private data class ValidatedAcknowledgement(
        val acknowledgementJson: String,
        val transferJson: String,
        val completionDetail: String,
    )

    private companion object {
        const val SCHEMA_VERSION = 1
        const val MODE_SAF_BACKUP = "saf_backup"
        const val MODE_SAF_RESTORE = "saf_restore"
        const val PENDING_PREFIX = "pending_"
        const val ACKNOWLEDGEMENT_PREFIX = "acknowledgement_"
        const val TRANSFER_PREFIX = "transfer_"
        val SAFE_JOB_ID = Regex("[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
        val LOWERCASE_SHA256 = Regex("[0-9a-f]{64}")
    }
}
