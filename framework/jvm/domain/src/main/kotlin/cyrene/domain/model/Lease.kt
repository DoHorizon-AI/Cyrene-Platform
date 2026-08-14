package cyrene.domain.model

import java.time.Instant

/**
 * Pure Kotlin domain model representing a fenced hardware resource lease.
 */
data class Lease(
    val leaseId: String,
    val nodeId: String,
    val resourceId: String,
    val fenceToken: Long,
    val generation: Long,
    val grantedAt: Instant,
    val expiresAt: Instant
) {
    fun isExpired(now: Instant): Boolean = now.isAfter(expiresAt)

    fun renew(newExpiresAt: Instant): Lease {
        require(newExpiresAt.isAfter(expiresAt)) {
            "New lease expiry $newExpiresAt must be after current expiry $expiresAt"
        }
        return copy(
            generation = generation + 1,
            expiresAt = newExpiresAt
        )
    }
}
