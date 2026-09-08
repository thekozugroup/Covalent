package life.michaelwong.covalent.sync

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.storage.StorageManager
import android.provider.Settings
import android.system.Os
import android.system.OsConstants
import java.io.File
import java.nio.file.Files
import java.nio.file.Paths
import java.text.Normalizer
import life.michaelwong.covalent.BuildConfig

internal data class RawFolderEntry(val name: String, val absolutePath: String)

/** Debug/personal-build gate for Syncthing's required ordinary filesystem capability. */
internal object FolderSyncSpecialAccess {
    fun supported(): Boolean = BuildConfig.DEBUG && Build.VERSION.SDK_INT >= 30

    fun granted(): Boolean {
        if (!BuildConfig.DEBUG || Build.VERSION.SDK_INT < 30) return false
        return Environment.isExternalStorageManager()
    }

    /** Constructed only from an explicit UI click; no startup code calls this method. */
    fun settingsIntent(context: Context): Intent {
        if (!BuildConfig.DEBUG || Build.VERSION.SDK_INT < 30) {
            throw IllegalStateException("Folder sync special access is unavailable in this build.")
        }
        return Intent(
            Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION,
            Uri.parse("package:${context.packageName}"),
        )
    }
}

/**
 * A bounded raw-directory browser. It accepts only NFC directories reached below a mounted shared
 * storage root with no symlink in the selected relative chain. It never translates SAF URIs.
 */
internal class RawFolderAccess(private val context: Context) {
    fun roots(): List<RawFolderEntry> {
        requireAccess()
        if (Build.VERSION.SDK_INT < 30) return emptyList()
        val manager = context.getSystemService(StorageManager::class.java)
            ?: throw IllegalStateException("Android shared storage is unavailable.")
        val volumes = manager.storageVolumes
        check(volumes.size <= MAX_VOLUMES) { "This device exposes too many storage volumes." }
        val roots = volumes.asSequence()
            .mapNotNull { it.directory }
            .map(::admitVolumeRoot)
            .distinctBy(RawFolderEntry::absolutePath)
            .toList()
        return roots
    }

    fun children(parent: String): List<RawFolderEntry> {
        val admitted = validate(parent, allowVolumeRoot = true)
        val values = mutableListOf<RawFolderEntry>()
        var inspected = 0
        runCatching {
            Files.newDirectoryStream(Paths.get(admitted)).use { stream ->
                for (childPath in stream) {
                    check(inspected < MAX_DIRECTORY_ENTRIES) {
                        "That folder contains too many entries to browse safely."
                    }
                    inspected += 1
                    val child = childPath.toFile()
                    val childName = child.name
                    check(
                        childName.isNotEmpty() && childName != "." && childName != ".." &&
                            '/' !in childName && '\\' !in childName && childName.none(Char::isISOControl) &&
                            Normalizer.isNormalized(childName, Normalizer.Form.NFC),
                    ) { "Choose another folder or rename this folder first." }
                    val isDirectory = runCatching {
                        val stat = Os.lstat(child.path)
                        OsConstants.S_ISDIR(stat.st_mode) && !OsConstants.S_ISLNK(stat.st_mode)
                    }.getOrElse { throw IllegalStateException("Android could not safely inspect that folder.") }
                    if (isDirectory) {
                        values += RawFolderEntry(childName, lexicalAbsolute(child.path))
                    }
                }
            }
        }.getOrElse {
            if (it is IllegalStateException) throw it
            throw IllegalStateException("Android could not read that folder.")
        }
        return values.sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.name })
    }

    /** Returns the same path after descriptor and component validation; it never rewrites it. */
    fun select(path: String): String = validate(path, allowVolumeRoot = false)

    private fun validate(path: String, allowVolumeRoot: Boolean): String {
        requireAccess()
        if (Build.VERSION.SDK_INT < 30) {
            throw IllegalStateException("Folder sync special access is unavailable in this build.")
        }
        val selected = lexicalAbsolute(path)
        check(Normalizer.isNormalized(selected, Normalizer.Form.NFC)) {
            "Choose another folder or rename this folder first."
        }
        val root = roots().map(RawFolderEntry::absolutePath)
            .filter { selected == it || selected.startsWith("$it/") }
            .maxByOrNull(String::length)
            ?: throw IllegalArgumentException("Choose a folder inside shared storage.")
        check(allowVolumeRoot || selected != root) { "Choose a folder inside this storage volume." }
        val relative = Paths.get(root).relativize(Paths.get(selected))
        check(relative.firstOrNull()?.toString() != "Android") {
            "Android's managed application directory cannot be shared."
        }
        var current = root
        relative.forEach { component ->
            val name = component.toString()
            check(name.isNotEmpty() && name != "." && name != ".." && '/' !in name && '\\' !in name)
            current += "/$name"
            val stat = runCatching { Os.lstat(current) }
                .getOrElse { throw IllegalArgumentException("Android cannot safely open that folder.") }
            check(OsConstants.S_ISDIR(stat.st_mode) && !OsConstants.S_ISLNK(stat.st_mode)) {
                "Folder sync does not follow links or non-directory entries."
            }
        }
        val descriptor = runCatching {
            Os.open(
                selected,
                OsConstants.O_RDONLY or OsConstants.O_DIRECTORY or O_CLOEXEC or
                    OsConstants.O_NOFOLLOW or OsConstants.O_NONBLOCK,
                0,
            )
        }.getOrElse { throw IllegalArgumentException("Android cannot safely open that folder.") }
        try {
            check(OsConstants.S_ISDIR(Os.fstat(descriptor).st_mode)) { "The selected entry is not a folder." }
            check(Os.fcntlInt(descriptor, OsConstants.F_GETFD, 0) and OsConstants.FD_CLOEXEC != 0) {
                "Android cannot safely retain that folder handle."
            }
        } finally {
            Os.close(descriptor)
        }
        return selected
    }

    private fun admitVolumeRoot(directory: File): RawFolderEntry {
        val path = lexicalAbsolute(directory.path)
        val stat = Os.lstat(path)
        check(OsConstants.S_ISDIR(stat.st_mode) && !OsConstants.S_ISLNK(stat.st_mode))
        return RawFolderEntry(directory.name.ifBlank { path }, path)
    }

    private fun requireAccess() {
        check(FolderSyncSpecialAccess.granted()) {
            "Allow all-files access from Android settings before choosing a sync folder."
        }
    }

    private fun lexicalAbsolute(value: String): String {
        check(value.length in 1..MAX_PATH_CHARS && value.none(Char::isISOControl) && value.startsWith('/')) {
            "The selected folder path is invalid."
        }
        val normalized = Paths.get(value).normalize().toString()
        check(normalized == value.trimEnd('/') || (value == "/" && normalized == value)) {
            "Choose another folder or rename this folder first."
        }
        return normalized
    }

    private companion object {
        const val MAX_PATH_CHARS = 4_096
        const val MAX_DIRECTORY_ENTRIES = 4_096
        const val MAX_VOLUMES = 16

        // O_CLOEXEC is 0x80000 in Android's asm-generic fcntl ABI, including API 26.
        // OsConstants.O_CLOEXEC is only exposed from API 27, so use the stable ABI value and
        // verify FD_CLOEXEC on the returned descriptor above.
        const val O_CLOEXEC = 0x80000
    }
}
