package life.michaelwong.covalent.node

import android.content.Context
import android.content.pm.ApplicationInfo
import android.os.Build
import android.os.Process
import android.system.Os
import android.system.OsConstants
import java.io.File
import java.io.FileInputStream
import java.security.MessageDigest

/** Host-verified immutable helpers. Paths contain no credential or user-entered value. */
internal data class VerifiedPackagedSyncEngine(
    val guardianPath: String,
    val guardianSha256: String,
    val workerPath: String,
    val workerSha256: String,
    val runtimeDirectory: String,
)

internal sealed interface PackagedSyncEnginePackage {
    data object Absent : PackagedSyncEnginePackage
    data object Invalid : PackagedSyncEnginePackage
    data class Verified(val engine: VerifiedPackagedSyncEngine) : PackagedSyncEnginePackage
}

internal data class PackagedHelperHash(
    val abi: String,
    val sha256: String,
    val bytes: Long,
)

/**
 * Opens the installer-extracted helpers and checks their packaged hash manifests.
 *
 * A developer build that deliberately omits the engine reports [PackagedSyncEnginePackage.Absent].
 * A production build with missing or damaged engine material reports
 * [PackagedSyncEnginePackage.Invalid], allowing the backup node to start while folder sync
 * remains visibly unavailable. Rust independently reopens and hashes both paths immediately
 * before launching them.
 */
internal object PackagedSyncEngine {
    private const val WORKER = "libsyncthing.so"
    private const val GUARDIAN = "libengineguardian.so"
    private const val WORKER_MANIFEST = "syncthing-sha256.txt"
    private const val GUARDIAN_MANIFEST = "engine-guardian-sha256.txt"
    private const val MAX_MANIFEST_BYTES = 1024
    private const val MAX_HELPER_BYTES = 64L * 1024L * 1024L
    private const val FILE_TYPE_MASK = 0xf000
    private const val DIRECTORY_MODE = 0x4000
    private const val OWNER_ONLY_MODE = 0x1c0
    private const val WORLD_WRITE_MODE = 0x2
    // The Java O_CLOEXEC field arrived in API 27; the flag already exists in
    // Android 8's arm64/x86_64 Linux ABI (asm-generic/fcntl.h: 02000000).
    // Pass it atomically to open on API 26 too, avoiding an open/fcntl race.
    private const val OPEN_CLOSE_ON_EXEC = 0x80000
    // Rust appends `/cv-engine-XXXXXX/api.sock` (26 bytes) below the Unix
    // socket's 100-byte portable path ceiling.
    private const val MAX_RUNTIME_PATH_BYTES = 74
    private val packagedAbis = setOf("arm64-v8a", "x86_64")
    private val hashPattern = Regex("[0-9a-f]{64}")

    fun load(context: Context): PackagedSyncEnginePackage = load(context) { }

    /** Debug-only callers use [onInvalidStage] to identify a fixed, nonsecret verification stage. */
    internal fun load(
        context: Context,
        onInvalidStage: (String) -> Unit,
    ): PackagedSyncEnginePackage {
        if (!life.michaelwong.covalent.BuildConfig.COVALENT_SYNC_ENGINE_PACKAGED) {
            return PackagedSyncEnginePackage.Absent
        }
        var stage = "extraction-metadata"
        return runCatching {
            val applicationContext = context.applicationContext
            check(
                applicationContext.applicationInfo.flags and
                    ApplicationInfo.FLAG_EXTRACT_NATIVE_LIBS != 0,
            )
            val workerHashes = readManifest(applicationContext, WORKER_MANIFEST) {
                stage = "worker-manifest-$it"
            }
            val guardianHashes = readManifest(applicationContext, GUARDIAN_MANIFEST) {
                stage = "guardian-manifest-$it"
            }
            stage = "device-abi"
            val abi = Build.SUPPORTED_ABIS.firstOrNull {
                workerHashes.containsKey(it) && guardianHashes.containsKey(it)
            } ?: error("No packaged ABI matches this device.")
            stage = "native-directory"
            val nativeDirectory = File(
                checkNotNull(applicationContext.applicationInfo.nativeLibraryDir),
            ).canonicalFile
            check(nativeDirectory.isDirectory)
            val worker = verifyHelper(
                nativeDirectory,
                WORKER,
                checkNotNull(workerHashes[abi]),
            ) { stage = "worker-helper-$it" }
            val guardian = verifyHelper(
                nativeDirectory,
                GUARDIAN,
                checkNotNull(guardianHashes[abi]),
            ) { stage = "guardian-helper-$it" }
            stage = "runtime-directory"
            val runtime = privateRuntimeDirectory(applicationContext)
            PackagedSyncEnginePackage.Verified(
                VerifiedPackagedSyncEngine(
                    guardianPath = guardian.path,
                    guardianSha256 = checkNotNull(guardianHashes[abi]).sha256,
                    workerPath = worker.path,
                    workerSha256 = checkNotNull(workerHashes[abi]).sha256,
                    runtimeDirectory = runtime.path,
                ),
            )
        }.getOrElse {
            onInvalidStage(stage)
            PackagedSyncEnginePackage.Invalid
        }
    }

    internal fun parseHashManifest(value: String): Map<String, PackagedHelperHash> {
        check(value.endsWith("\n") && '\r' !in value)
        val lines = value.dropLast(1).split('\n')
        check(lines.size == packagedAbis.size)
        val rows = lines.map { line ->
            val parts = line.split(' ')
            check(parts.size == 3 && parts.none(String::isBlank))
            val abi = parts[0]
            val hash = parts[1]
            val bytes = checkNotNull(parts[2].toLongOrNull())
            check(abi in packagedAbis && hashPattern.matches(hash))
            check(bytes in 1..MAX_HELPER_BYTES)
            check(bytes.toString() == parts[2])
            PackagedHelperHash(abi, hash, bytes)
        }
        check(rows.map(PackagedHelperHash::abi) == listOf("arm64-v8a", "x86_64"))
        return rows.associateBy(PackagedHelperHash::abi)
    }

    /**
     * Android creates app-private roots such as `noBackupFilesDir` with mode 0771.
     * The group is still the app sandbox identity; reject foreign groups and world-write access.
     * The dedicated engine runtime directory below this root remains exact mode 0700.
     */
    internal fun privateRuntimeParentAllowed(
        mode: Int,
        uid: Int,
        gid: Int,
        appUid: Int,
    ): Boolean =
        mode and FILE_TYPE_MASK == DIRECTORY_MODE &&
            uid == appUid &&
            gid == appUid &&
            mode and WORLD_WRITE_MODE == 0

    internal fun privateRuntimeChildAllowed(mode: Int, uid: Int, appUid: Int): Boolean =
        mode and FILE_TYPE_MASK == DIRECTORY_MODE &&
            uid == appUid &&
            mode and 0b111_111_111 == OWNER_ONLY_MODE

    internal fun runtimeParentPathFits(value: String): Boolean =
        value.toByteArray(Charsets.UTF_8).size <= MAX_RUNTIME_PATH_BYTES

    private fun readManifest(
        context: Context,
        name: String,
        onStage: (String) -> Unit,
    ): Map<String, PackagedHelperHash> {
        onStage("open")
        val bytes = context.assets.open(name).use { input ->
            onStage("bounded-read")
            val retained = ByteArray(MAX_MANIFEST_BYTES + 1)
            var offset = 0
            while (offset < retained.size) {
                val count = input.read(retained, offset, retained.size - offset)
                if (count < 0) break
                check(count > 0)
                offset += count
            }
            check(offset <= MAX_MANIFEST_BYTES && input.read() < 0)
            retained.copyOf(offset)
        }
        onStage("parse")
        return parseHashManifest(bytes.toString(Charsets.US_ASCII))
    }

    private fun verifyHelper(
        nativeDirectory: File,
        name: String,
        expected: PackagedHelperHash,
        onStage: (String) -> Unit,
    ): File {
        val helper = File(nativeDirectory, name).absoluteFile
        onStage("direct-child")
        check(helper.parentFile == nativeDirectory)
        onStage("lstat")
        val before = Os.lstat(helper.path)
        onStage("regular")
        check(OsConstants.S_ISREG(before.st_mode))
        onStage("canonical")
        check(helper.canonicalFile == helper)
        onStage("size")
        check(before.st_size == expected.bytes)
        onStage("mode")
        check(before.st_mode and (OsConstants.S_IWGRP or OsConstants.S_IWOTH) == 0)
        onStage("executable")
        check(Os.access(helper.path, OsConstants.X_OK))
        val digest = MessageDigest.getInstance("SHA-256")
        onStage("open")
        val descriptor = Os.open(
            helper.path,
            OsConstants.O_RDONLY or OPEN_CLOSE_ON_EXEC or
                OsConstants.O_NOFOLLOW or OsConstants.O_NONBLOCK,
            0,
        )
        FileInputStream(descriptor).use { input ->
            // The atomic open flag is supported on every packaged ABI. Android
            // exposes this additional descriptor assertion only from API 30.
            if (Build.VERSION.SDK_INT >= 30) {
                onStage("close-on-exec")
                check(Os.fcntlInt(input.fd, OsConstants.F_GETFD, 0) and OsConstants.FD_CLOEXEC != 0)
            }
            onStage("fstat-identity")
            val opened = Os.fstat(input.fd)
            check(
                opened.st_dev == before.st_dev &&
                    opened.st_ino == before.st_ino &&
                    OsConstants.S_ISREG(opened.st_mode),
            )
            val buffer = ByteArray(64 * 1024)
            var retained = 0L
            onStage("bounded-read")
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                check(count > 0)
                retained += count
                check(retained <= expected.bytes)
                digest.update(buffer, 0, count)
            }
            check(retained == expected.bytes)
            onStage("post-read-identity")
            val afterRead = Os.fstat(input.fd)
            check(
                afterRead.st_dev == opened.st_dev &&
                    afterRead.st_ino == opened.st_ino &&
                    afterRead.st_size == opened.st_size,
            )
            onStage("close")
        }
        onStage("hash")
        check(digest.digest().toHex() == expected.sha256)
        onStage("post-read-path-identity")
        val after = Os.lstat(helper.path)
        check(
            after.st_dev == before.st_dev &&
                after.st_ino == before.st_ino &&
                after.st_size == before.st_size &&
                OsConstants.S_ISREG(after.st_mode),
        )
        return helper
    }

    private fun privateRuntimeDirectory(context: Context): File {
        val privateRoot = context.noBackupFilesDir.canonicalFile
        val rootMetadata = Os.lstat(privateRoot.path)
        check(
            privateRuntimeParentAllowed(
                rootMetadata.st_mode,
                rootMetadata.st_uid,
                rootMetadata.st_gid,
                Process.myUid(),
            ),
        )
        val directory = File(privateRoot, "s").absoluteFile
        check(directory.parentFile == privateRoot)
        try {
            Os.mkdir(directory.path, OsConstants.S_IRWXU)
        } catch (error: android.system.ErrnoException) {
            if (error.errno != OsConstants.EEXIST) throw error
        }
        val metadata = Os.lstat(directory.path)
        check(
            privateRuntimeChildAllowed(metadata.st_mode, metadata.st_uid, Process.myUid()),
        )
        check(directory.canonicalFile == directory)
        check(runtimeParentPathFits(directory.path))
        return directory
    }

    private fun ByteArray.toHex(): String = joinToString("") { "%02x".format(it) }
}
