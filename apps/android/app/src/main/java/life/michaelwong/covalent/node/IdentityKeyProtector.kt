package life.michaelwong.covalent.node

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.util.AtomicFile
import android.util.Base64
import android.util.Log
import androidx.annotation.RequiresApi
import java.io.File
import java.io.FileOutputStream
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec

/** Fixed JNI wire values for the measured Android Keystore protection level. */
enum class KeyProtectionLevel(val wireValue: Int) {
    UNAVAILABLE(0),
    SOFTWARE(1),
    TRUSTED_ENVIRONMENT(2),
    STRONGBOX(3),
    ;

    val hardwareBacked: Boolean get() = this == TRUSTED_ENVIRONMENT || this == STRONGBOX
}

/**
 * Owned, versioned KEK plaintext. The caller must call [close] in a `finally` block.
 *
 * The only persisted representation of these bytes is the AES-GCM ciphertext written to
 * `noBackupFilesDir/covalent-node-secrets/kek-envelope.v1`.
 */
internal class VersionedKeyEncryptionKey(
    val version: Int,
    val bytes: ByteArray,
) : AutoCloseable {
    override fun close() {
        bytes.fill(0)
    }
}

/**
 * Non-exportable Android Keystore wrapper for the embedded node's exact versioned KEK.
 *
 * The Android key never leaves `AndroidKeyStore`. A random 32-byte Covalent KEK is wrapped
 * with AES-256-GCM and stored atomically under [Context.getNoBackupFilesDir]. The KEK is
 * unwrapped only for a synchronous JNI start and every owned Kotlin/JNI/Rust input buffer is
 * zeroized. Rust retains its own zeroizing `StaticKeyProtector` only while the node is live.
 *
 * Existing ciphertext is authoritative. If the Keystore entry is missing, invalidated,
 * authentication fails, the envelope is malformed, or any I/O fails, no replacement key is
 * generated and the provider stays locked. Minting a new KEK is allowed only when no KEK
 * envelope exists; that is the legacy local-node migration case. The core then atomically
 * rewrites legacy plaintext identity/TLS/key records under this KEK.
 *
 * The key is intentionally not user-authentication-bound. The opted-in provider must run in
 * a foreground service while the screen is locked. Credential-encrypted no-backup storage
 * still keeps the envelope unavailable before the first unlock after reboot.
 */
internal class IdentityKeyProtector(context: Context) {
    private val applicationContext = context.applicationContext
    // Resolve credential-encrypted no-backup storage only after DirectBoot admission.
    private val kekEnvelope by lazy {
        AtomicFile(File(applicationContext.noBackupFilesDir, "covalent-node-secrets/kek-envelope.v1"))
    }
    private val csprng = SecureRandom()

    fun protection(): KeyProtectionLevel {
        if (!DirectBoot.isUserUnlocked(applicationContext)) return KeyProtectionLevel.UNAVAILABLE
        cachedProtection?.let { return it }
        val measured = synchronized(lock) { measureProtection() }
        if (measured != KeyProtectionLevel.UNAVAILABLE) cachedProtection = measured
        return measured
    }

    /**
     * Opens the persisted KEK or creates it exactly once for a fresh/legacy local node.
     * Existing-but-unopenable ciphertext always returns null and is never replaced.
     */
    fun loadOrCreateKeyEncryptionKey(): VersionedKeyEncryptionKey? = synchronized(lock) {
        if (!DirectBoot.isUserUnlocked(applicationContext)) return@synchronized null
        if (kekEnvelope.baseFile.exists()) return@synchronized openPersistedKek()

        val generated = ByteArray(KEK_BYTES)
        try {
            csprng.nextBytes(generated)
            val sealed = seal(generated, KEK_AAD) ?: return@synchronized null
            if (!writeKekEnvelope("$KEK_ENVELOPE_VERSION:$CURRENT_KEK_VERSION:$sealed")) {
                return@synchronized null
            }
            // Read through the durable representation. This proves the bytes that will be
            // used after the next process start are the exact bytes passed to Rust now.
            openPersistedKek()
        } finally {
            generated.fill(0)
        }
    }

    /** Seals a local-node API token under a separate authenticated domain. */
    fun sealToken(plaintext: ByteArray): String? = synchronized(lock) {
        seal(plaintext, TOKEN_AAD)?.let { "$TOKEN_ENVELOPE_VERSION:$it" }
    }

    /** Opens only the current token envelope. Every error is a locked result. */
    fun openToken(envelope: String): ByteArray? = synchronized(lock) {
        val parts = envelope.split(SEPARATOR)
        if (parts.size != 3 || parts[0] != TOKEN_ENVELOPE_VERSION) return@synchronized null
        open(parts[1], parts[2], TOKEN_AAD, requireCurrentKey(allowCreate = false))
    }

    /**
     * Opens the pre-v2 local-token envelope without modifying it. The caller atomically
     * replaces the preference with [sealToken] only after this succeeds.
     */
    fun openLegacyToken(envelope: String): ByteArray? = synchronized(lock) {
        val parts = envelope.split(SEPARATOR)
        if (parts.size != 2) return@synchronized null
        val legacy = keyStore().getKey(LEGACY_TOKEN_KEY_ALIAS, null) as? SecretKey
            ?: return@synchronized null
        open(parts[0], parts[1], byteArrayOf(), legacy)
    }

    private fun openPersistedKek(): VersionedKeyEncryptionKey? {
        val encoded = runCatching {
            val file = kekEnvelope.baseFile
            if (!file.isFile || file.length() !in 1..MAX_ENVELOPE_BYTES.toLong()) return null
            kekEnvelope.openRead().bufferedReader(Charsets.UTF_8).use { it.readText() }
        }.getOrElse { failure ->
            Log.w(TAG, "Reading the protected node key failed: ${failure.javaClass.simpleName}")
            return null
        }
        val parts = encoded.split(SEPARATOR)
        if (parts.size != 4 || parts[0] != KEK_ENVELOPE_VERSION) return null
        val version = parts[1].toIntOrNull()?.takeIf { it == CURRENT_KEK_VERSION } ?: return null
        val plaintext = open(
            iv = parts[2],
            ciphertext = parts[3],
            aad = kekAad(version),
            key = requireCurrentKey(allowCreate = false),
        ) ?: return null
        if (plaintext.size != KEK_BYTES) {
            plaintext.fill(0)
            return null
        }
        return VersionedKeyEncryptionKey(version, plaintext)
    }

    private fun measureProtection(): KeyProtectionLevel = runCatching {
        val key = requireCurrentKey(allowCreate = !hasCurrentProtectedMaterial())
            ?: return KeyProtectionLevel.UNAVAILABLE
        val probe = PROBE.copyOf()
        val opened = try {
            val sealed = seal(probe, PROBE_AAD, key) ?: return KeyProtectionLevel.UNAVAILABLE
            val parts = sealed.split(SEPARATOR)
            if (parts.size != 2) return KeyProtectionLevel.UNAVAILABLE
            open(parts[0], parts[1], PROBE_AAD, key)
        } finally {
            probe.fill(0)
        }
        val roundTrip = try {
            opened != null && opened.contentEquals(PROBE)
        } finally {
            opened?.fill(0)
        }
        if (!roundTrip) return KeyProtectionLevel.UNAVAILABLE
        securityLevel(key)
    }.getOrElse { failure ->
        Log.w(TAG, "Keystore key protection is unavailable: ${failure.javaClass.simpleName}")
        KeyProtectionLevel.UNAVAILABLE
    }

    /** Existing ciphertext means alias loss/invalidation must not silently mint a new key. */
    private fun hasCurrentProtectedMaterial(): Boolean {
        if (!DirectBoot.isUserUnlocked(applicationContext)) return true
        if (kekEnvelope.baseFile.exists()) return true
        return runCatching {
            applicationContext
                .getSharedPreferences(LOCAL_CREDENTIAL_PREFERENCES, Context.MODE_PRIVATE)
                .getString(TOKEN_PREFERENCE, null)
                ?.startsWith("$TOKEN_ENVELOPE_VERSION$SEPARATOR") == true
        }.getOrDefault(true)
    }

    private fun seal(plaintext: ByteArray, aad: ByteArray): String? {
        val key = requireCurrentKey(allowCreate = !hasCurrentProtectedMaterial()) ?: return null
        return seal(plaintext, aad, key)
    }

    private fun seal(plaintext: ByteArray, aad: ByteArray, key: SecretKey): String? = runCatching {
        val cipher = Cipher.getInstance(TRANSFORMATION).apply {
            init(Cipher.ENCRYPT_MODE, key)
            updateAAD(aad)
        }
        val ciphertext = cipher.doFinal(plaintext)
        try {
            Base64.encodeToString(cipher.iv, Base64.NO_WRAP) + SEPARATOR +
                Base64.encodeToString(ciphertext, Base64.NO_WRAP)
        } finally {
            ciphertext.fill(0)
        }
    }.getOrElse { failure ->
        Log.w(TAG, "Sealing protected node material failed: ${failure.javaClass.simpleName}")
        null
    }

    private fun open(
        iv: String,
        ciphertext: String,
        aad: ByteArray,
        key: SecretKey?,
    ): ByteArray? {
        if (key == null || iv.length > MAX_ENVELOPE_BYTES || ciphertext.length > MAX_ENVELOPE_BYTES) return null
        var decodedIv: ByteArray? = null
        var decodedCiphertext: ByteArray? = null
        return runCatching {
            decodedIv = Base64.decode(iv, Base64.NO_WRAP)
            decodedCiphertext = Base64.decode(ciphertext, Base64.NO_WRAP)
            require(decodedIv!!.size == GCM_IV_BYTES) { "invalid IV" }
            require(decodedCiphertext!!.size in GCM_TAG_BYTES..MAX_CIPHERTEXT_BYTES) { "invalid ciphertext" }
            Cipher.getInstance(TRANSFORMATION).apply {
                init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, decodedIv))
                updateAAD(aad)
            }.doFinal(decodedCiphertext)
        }.getOrElse { failure ->
            Log.w(TAG, "Opening protected node material failed: ${failure.javaClass.simpleName}")
            null
        }.also {
            decodedIv?.fill(0)
            decodedCiphertext?.fill(0)
        }
    }

    private fun writeKekEnvelope(value: String): Boolean {
        val parent = kekEnvelope.baseFile.parentFile ?: return false
        if (!(parent.mkdirs() || parent.isDirectory)) return false
        var output: FileOutputStream? = null
        val bytes = value.encodeToByteArray()
        return try {
            output = kekEnvelope.startWrite()
            output.write(bytes)
            output.fd.sync()
            kekEnvelope.finishWrite(output)
            output = null
            true
        } catch (failure: Exception) {
            output?.let(kekEnvelope::failWrite)
            Log.w(TAG, "Persisting the protected node key failed: ${failure.javaClass.simpleName}")
            false
        } finally {
            bytes.fill(0)
        }
    }

    private fun securityLevel(key: SecretKey): KeyProtectionLevel {
        val info = SecretKeyFactory.getInstance(key.algorithm, PROVIDER)
            .getKeySpec(key, KeyInfo::class.java) as KeyInfo
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            securityLevelFromApi31(info)
        } else {
            @Suppress("DEPRECATION")
            if (info.isInsideSecureHardware) KeyProtectionLevel.TRUSTED_ENVIRONMENT
            else KeyProtectionLevel.SOFTWARE
        }
    }

    @RequiresApi(Build.VERSION_CODES.S)
    private fun securityLevelFromApi31(info: KeyInfo): KeyProtectionLevel = when (info.securityLevel) {
        KeyProperties.SECURITY_LEVEL_STRONGBOX -> KeyProtectionLevel.STRONGBOX
        KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT,
        KeyProperties.SECURITY_LEVEL_UNKNOWN_SECURE,
        -> KeyProtectionLevel.TRUSTED_ENVIRONMENT
        else -> KeyProtectionLevel.SOFTWARE
    }

    private fun requireCurrentKey(allowCreate: Boolean): SecretKey? {
        val store = keyStore()
        (store.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        if (!allowCreate) return null
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            generateStrongBoxKey()?.let { return it }
        }
        return generateKey(strongBox = false)
    }

    @RequiresApi(Build.VERSION_CODES.P)
    private fun generateStrongBoxKey(): SecretKey? = try {
        generateKey(strongBox = true)
    } catch (_: StrongBoxUnavailableException) {
        null
    }

    private fun generateKey(strongBox: Boolean): SecretKey =
        KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER).apply {
            init(
                KeyGenParameterSpec.Builder(
                    KEY_ALIAS,
                    KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                )
                    .setKeySize(KEY_SIZE_BITS)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setRandomizedEncryptionRequired(true)
                    .setUserAuthenticationRequired(false)
                    .apply {
                        if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                            setIsStrongBoxBacked(true)
                        }
                    }
                    .build(),
            )
        }.generateKey()

    private fun keyStore(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

    private fun kekAad(version: Int): ByteArray =
        "covalent.node.kek-envelope/$version".encodeToByteArray()

    private companion object {
        const val TAG = "CovalentKeyProtector"
        const val PROVIDER = "AndroidKeyStore"
        const val KEY_ALIAS = "covalent.node.identity.protector.v2"
        const val LEGACY_TOKEN_KEY_ALIAS = "covalent.embedded.node.token.v1"
        const val LOCAL_CREDENTIAL_PREFERENCES = "covalent_embedded_node_credentials"
        const val TOKEN_PREFERENCE = "token"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val KEK_ENVELOPE_VERSION = "kek1"
        const val TOKEN_ENVELOPE_VERSION = "v2"
        const val SEPARATOR = ":"
        const val CURRENT_KEK_VERSION = 1
        const val KEK_BYTES = 32
        const val KEY_SIZE_BITS = 256
        const val GCM_TAG_BITS = 128
        const val GCM_TAG_BYTES = GCM_TAG_BITS / 8
        const val GCM_IV_BYTES = 12
        const val MAX_CIPHERTEXT_BYTES = 1024
        const val MAX_ENVELOPE_BYTES = 4096
        val PROBE = "covalent-key-protection-probe".encodeToByteArray()
        val PROBE_AAD = "covalent.node.key-protection-probe".encodeToByteArray()
        val TOKEN_AAD = "covalent.node.local-api-credential".encodeToByteArray()
        val KEK_AAD = "covalent.node.kek-envelope/1".encodeToByteArray()
        val lock = Any()

        @Volatile
        var cachedProtection: KeyProtectionLevel? = null
    }
}
