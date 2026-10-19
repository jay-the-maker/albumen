// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

enum class BackupDisposition {
    SUCCESS,
    RETRY,
    FAILURE,
}

/**
 * User-visible result of one backup pass. Every branch that stops short of a
 * verified copy adds a non-blank line. The summary is always present, so a
 * pass cannot end as an empty string.
 */
class BackupReport {
    private val lines = mutableListOf<String>()
    val failures = mutableListOf<String>()
    var verified: Int = 0
    var skipped: Int = 0
    var retry: Boolean = false
        private set
    var blocked: Boolean = false
        private set

    fun note(message: String) {
        require(message.isNotBlank()) { "status message is empty" }
        lines += message
    }

    fun fail(message: String, retryLater: Boolean) {
        require(message.isNotBlank()) { "failure message is empty" }
        failures += message
        lines += message
        if (retryLater) {
            retry = true
        }
    }

    fun block(message: String) {
        require(message.isNotBlank()) { "failure message is empty" }
        failures += message
        lines += message
        blocked = true
    }

    fun pause(message: String) {
        note(message)
        retry = true
    }

    fun summaryLine(): String =
        "Verified $verified this run. Already verified $skipped. Failures ${failures.size}."

    fun text(): String {
        val summary = summaryLine()
        val body = lines.joinToString("\n")
        return if (body.isBlank()) summary else "$body\n$summary"
    }

    fun disposition(): BackupDisposition = when {
        blocked -> BackupDisposition.FAILURE
        retry -> BackupDisposition.RETRY
        failures.isNotEmpty() -> BackupDisposition.FAILURE
        else -> BackupDisposition.SUCCESS
    }
}

fun failureText(throwable: Throwable): String =
    throwable.message?.takeIf { it.isNotBlank() } ?: throwable.javaClass.simpleName
