// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKeys

/**
 * Receiver password in storage backed by the Android Keystore.
 * WorkManager input does not receive this value.
 */
internal object ReceiverPasswordStore {
    private const val FILE = "albumen-receiver-password"
    private const val KEY = "password"

    fun save(context: Context, password: String) {
        check(password.isNotEmpty()) { "receiver password is empty" }
        prefs(context).edit().putString(KEY, password).commit()
    }

    fun read(context: Context): String? {
        val stored = prefs(context).getString(KEY, null)
        return stored?.takeIf { it.isNotEmpty() }
    }

    private fun prefs(context: Context): SharedPreferences {
        val masterKeyAlias = MasterKeys.getOrCreate(MasterKeys.AES256_GCM_SPEC)
        return EncryptedSharedPreferences.create(
            FILE,
            masterKeyAlias,
            context,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }
}
