// SPDX-License-Identifier: Apache-2.0

package photos.albumen.backup

/**
 * Seeded fault for one synthetic backup item. The same bit operations are
 * in the iOS simulator soak. A pull request checks the shared vector.
 *
 * The first four items of a shard are one of each fault, so a short smoke
 * still sees every kind. Later items are healthy about four times out of five.
 */
object ScaleSoakSchedule {
    val order: List<String> = listOf(
        "network-drop",
        "power-loss",
        "receiver-down",
        "sha-mismatch",
    )

    fun fault(seed: Long, localIndex: Int, globalIndex: Int): String {
        if (localIndex < order.size) {
            return order[localIndex]
        }
        val slot = (mix(seed, globalIndex) ushr 1) % 20L
        if (slot < order.size) {
            return order[slot.toInt()]
        }
        return "healthy"
    }

    private fun mix(seed: Long, globalIndex: Int): Long {
        val add = java.lang.Long.parseUnsignedLong("9E3779B97F4A7C15", 16)
        val mul1 = java.lang.Long.parseUnsignedLong("BF58476D1CE4E5B9", 16)
        val mul2 = java.lang.Long.parseUnsignedLong("94D049BB133111EB", 16)
        var state = seed xor (globalIndex.toLong() * add)
        state += add
        var z = state
        z = (z xor (z ushr 30)) * mul1
        z = (z xor (z ushr 27)) * mul2
        return z xor (z ushr 31)
    }
}
