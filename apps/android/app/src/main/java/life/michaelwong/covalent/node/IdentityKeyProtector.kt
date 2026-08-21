package life.michaelwong.covalent.node

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.util.Base64
import android.util.Log
import androidx.annotation.RequiresApi
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec

/**
 * How well this device can protect the credential that guards its Covalent identity.
 *
 * [wireValue] is a fixed part of the JNI contract and is decoded by
 * `IdentityProtection::from_wire` in `crates/covalent-android-jni`. Rust fails closed on
 * [UNAVAILABLE] and on any value it does not recognise, so these numbers must not be
 * reassigned; new levels may only be appended.
 */
enum class KeyProtectionLevel(val wireValue: Int) {
    /** No Keystore key could be created or exercised. The embedded node must not run. */
    UNAVAILABLE(0),

    /** A Keystore key exists, but this platform keeps it in software. */
    SOFTWARE(1),

    /** The key lives in the device's trusted execution environment. */
    TRUSTED_ENVIRONMENT(2),

    /** The key lives in a discrete StrongBox security chip. */
    STRONGBOX(3),
    ;

    /** True when the key material is held by dedicated secure hardware. */
    val hardwareBacked: Boolean get() = this == TRUSTED_ENVIRONMENT || this == STRONGBOX
}

/**
 * Android Keystore protector for the embedded node's local API credential.
 *
 * ## What this protects, and what it does not
 *
 * The embedded node keeps two long-lived secrets. This class owns the first outright and
 * gates the second:
 *
 *  1. **The local API bearer token.** Every management operation on the embedded node
 *     authenticates with it. It is generated here, sealed under a non-exportable
 *     AES-256-GCM key inside `AndroidKeyStore`, and only ever handed to Rust as a
 *     transient byte array that both sides zero after use.
 *  2. **The node's TLS identity keypair**, written by `covalent-node` into
 *     `data_directory/tls`. `covalent-node` owns that file directly and exposes no hook
 *     for wrapping it, so it is protected by the app-private UID sandbox plus Android
 *     file-based encryption rather than by a Keystore key. **It is not hardware-bound.**
 *     Closing that gap needs an identity-protector seam inside `crates/covalent-node`.
 *     Until it exists, [KeyProtectionLevel] describes the credential this class holds,
 *     and admission is fail-closed on it: no Keystore key, no embedded node, so the TLS
 *     identity is never created on a device that cannot protect anything at all.
 *
 * ## Key lifecycle decisions
 *
 * **Generation.** One AES-256-GCM key per install, alias [KEY_ALIAS], created lazily on
 * first use. StrongBox is requested first on API 28+ and the generation is retried
 * without it when the device has no StrongBox. `setRandomizedEncryptionRequired(true)`
 * forces a platform-generated IV per seal, so a caller cannot reuse one by mistake.
 *
 * **Storage.** The key is non-exportable: it never leaves the Keystore, and on a
 * hardware-backed device it never leaves secure hardware. Only the sealed envelope is
 * written to `SharedPreferences`, and the app's `allowBackup` is off, so no envelope
 * leaves the device in a cloud backup where its key could not follow.
 *
 * **Use.** No `setUserAuthenticationRequired(true)`. This is deliberate and is the one
 * decision here that trades security for function: the node runs headless in a foreground
 * service and in scheduled `WorkManager` transfers, including while the screen is locked.
 * An auth-bound key would make every unattended backup fail. Because the key is not
 * auth-bound, `setInvalidatedByBiometricEnrollment` has no effect on it — that flag only
 * governs auth-bound keys — so enrolling or removing a fingerprint does **not** invalidate
 * this key and does not interrupt backups. The trade-off: an attacker with a running,
 * unlocked device and code execution as this app can use the key, though still not
 * extract it.
 *
 * **Rotation.** The sealed envelope carries a version tag ([ENVELOPE_VERSION]). Any
 * failure to open an envelope — a replaced key, a corrupted record, a format change — is
 * treated as "this credential is gone": the caller mints a fresh token and re-seals it.
 * That is safe because the token is a loopback-only bearer credential that is always
 * passed explicitly to the node at start, so rotating it invalidates nothing durable. The
 * TLS identity is deliberately *not* rotated with it, because rotating it would break
 * every pairing this device has completed.
 *
 * **Device loss.** The key is non-exportable and, on hardware-backed devices, bound to
 * the device's secure hardware. Removing the storage does not yield a usable key.
 *
 * **App uninstall.** Android deletes the app's Keystore entries along with the app. The
 * node's state directory lives under `noBackupFilesDir`, which is deleted at the same
 * time, so the credential and the data it guarded disappear together.
 *
 * **Factory reset.** Erases the Keystore, including hardware-backed key material. Any
 * surviving copy of the node directory becomes permanently unopenable, which is the
 * intended outcome.
 *
 * **Lockscreen change.** Clearing the device lock wipes auth-bound keys on most Android
 * versions. This key is not auth-bound, so it normally survives. Some vendors are
 * stricter, so [KeyPermanentlyInvalidatedException] is still handled: the alias is
 * dropped and the credential rotates on the next start.
 */
internal class IdentityKeyProtector {

    /**
     * The measured protection level for this device.
     *
     * This probes rather than assumes: it creates the key if needed, performs a real
     * seal/open round trip through it, and only then reports the level recorded in the
     * key's own [KeyInfo]. A key that exists but cannot actually encrypt reports
     * [KeyProtectionLevel.UNAVAILABLE].
     */
    fun protection(): KeyProtectionLevel {
        cached?.let { return it }
        val measured = measureProtection()
        if (measured != KeyProtectionLevel.UNAVAILABLE) cached = measured
        return measured
    }

    /** Seals [plaintext] under the Keystore key, or returns null if the key is unusable. */
    fun seal(plaintext: String): String? = runCatching {
        val cipher = Cipher.getInstance(TRANSFORMATION).apply {
            init(Cipher.ENCRYPT_MODE, requireKey())
            updateAAD(AAD)
        }
        val ciphertext = cipher.doFinal(plaintext.encodeToByteArray())
        listOf(
            ENVELOPE_VERSION,
            Base64.encodeToString(cipher.iv, Base64.NO_WRAP),
            Base64.encodeToString(ciphertext, Base64.NO_WRAP),
        ).joinToString(SEPARATOR)
    }.getOrElse { failure ->
        forgetIfPermanentlyInvalidated(failure)
        Log.w(TAG, "Sealing the local node credential failed: ${failure.javaClass.simpleName}")
        null
    }

    /**
     * Opens an envelope produced by [seal]. Returns null for every failure — a rotated
     * key, a corrupted record, an envelope from an older format — so callers uniformly
     * treat an unopenable credential as one that must be minted again.
     */
    fun open(envelope: String): String? = runCatching {
        val parts = envelope.split(SEPARATOR)
        require(parts.size == 3 && parts[0] == ENVELOPE_VERSION) { "unsupported envelope" }
        val cipher = Cipher.getInstance(TRANSFORMATION).apply {
            init(
                Cipher.DECRYPT_MODE,
                requireKey(),
                GCMParameterSpec(GCM_TAG_BITS, Base64.decode(parts[1], Base64.NO_WRAP)),
            )
            updateAAD(AAD)
        }
        cipher.doFinal(Base64.decode(parts[2], Base64.NO_WRAP)).decodeToString()
    }.getOrElse { failure ->
        forgetIfPermanentlyInvalidated(failure)
        Log.w(TAG, "Opening the local node credential failed: ${failure.javaClass.simpleName}")
        null
    }

    /** Drops the Keystore entry so the next call regenerates it. */
    fun forget() {
        cached = null
        runCatching { keyStore().deleteEntry(KEY_ALIAS) }
    }

    private fun measureProtection(): KeyProtectionLevel = runCatching {
        val key = requireKey()
        // A round trip through the real key, because a key can exist and still be
        // unusable: a wiped secure element, a revoked entry, a broken provider.
        val probe = seal(PROBE_PLAINTEXT) ?: return KeyProtectionLevel.UNAVAILABLE
        if (open(probe) != PROBE_PLAINTEXT) return KeyProtectionLevel.UNAVAILABLE
        securityLevel(key)
    }.getOrElse { failure ->
        forgetIfPermanentlyInvalidated(failure)
        Log.w(TAG, "Keystore identity protection is unavailable: ${failure.javaClass.simpleName}")
        KeyProtectionLevel.UNAVAILABLE
    }

    private fun securityLevel(key: SecretKey): KeyProtectionLevel {
        val info = SecretKeyFactory.getInstance(key.algorithm, PROVIDER)
            .getKeySpec(key, KeyInfo::class.java) as KeyInfo
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            securityLevelFromApi31(info)
        } else {
            // API 26-30 has no KeyInfo.getSecurityLevel(), so this deprecated predicate is
            // the only way to tell a TEE-backed key from a software one on those releases.
            // Dropping it would report SOFTWARE for every pre-31 device and understate the
            // hardware protection that is actually there. Delete this branch — and the
            // CodeQL java/deprecated-call dismissal covering it — when minSdk reaches 31.
            @Suppress("DEPRECATION")
            if (info.isInsideSecureHardware) {
                KeyProtectionLevel.TRUSTED_ENVIRONMENT
            } else {
                KeyProtectionLevel.SOFTWARE
            }
        }
    }

    @RequiresApi(Build.VERSION_CODES.S)
    private fun securityLevelFromApi31(info: KeyInfo): KeyProtectionLevel =
        when (info.securityLevel) {
            KeyProperties.SECURITY_LEVEL_STRONGBOX -> KeyProtectionLevel.STRONGBOX
            KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT -> KeyProtectionLevel.TRUSTED_ENVIRONMENT
            // UNKNOWN_SECURE means the platform vouches for secure hardware without
            // naming it; anything else, including SECURITY_LEVEL_UNKNOWN, is reported
            // as software so the UI never overstates what protects the credential.
            KeyProperties.SECURITY_LEVEL_UNKNOWN_SECURE -> KeyProtectionLevel.TRUSTED_ENVIRONMENT
            else -> KeyProtectionLevel.SOFTWARE
        }

    private fun requireKey(): SecretKey {
        val store = keyStore()
        (store.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            generateStrongBoxKey()?.let { return it }
        }
        return generateKey(strongBox = false)
    }

    @RequiresApi(Build.VERSION_CODES.P)
    private fun generateStrongBoxKey(): SecretKey? = try {
        generateKey(strongBox = true)
    } catch (_: StrongBoxUnavailableException) {
        // Expected on every device without a StrongBox chip.
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
                    .apply {
                        if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                            setIsStrongBoxBacked(true)
                        }
                    }
                    .build(),
            )
        }.generateKey()

    private fun keyStore(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

    private fun forgetIfPermanentlyInvalidated(failure: Throwable) {
        if (failure is KeyPermanentlyInvalidatedException) forget()
    }

    private companion object {
        const val TAG = "CovalentIdentityKey"
        const val PROVIDER = "AndroidKeyStore"

        /**
         * A new alias, distinct from the pre-Keystore-hardening `…token.v1` entry. The
         * old alias was generated without an explicit key size, without randomized
         * encryption, and without a StrongBox attempt; adopting a fresh alias means an
         * upgrading install gets the hardened key rather than silently keeping the weaker
         * one, at the cost of one credential rotation.
         */
        const val KEY_ALIAS = "covalent.node.identity.protector.v2"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val ENVELOPE_VERSION = "v2"
        const val SEPARATOR = ":"
        const val KEY_SIZE_BITS = 256
        const val GCM_TAG_BITS = 128
        const val PROBE_PLAINTEXT = "covalent-key-protection-probe"
        val AAD = "covalent.node.local-api-credential".encodeToByteArray()

        /**
         * Probing costs a Keystore round trip, so a successful measurement is reused for
         * the process lifetime. Only successes are cached: a device that failed once —
         * during early boot, or while the secure element was busy — must be able to
         * recover without a restart.
         */
        @Volatile
        var cached: KeyProtectionLevel? = null
    }
}
