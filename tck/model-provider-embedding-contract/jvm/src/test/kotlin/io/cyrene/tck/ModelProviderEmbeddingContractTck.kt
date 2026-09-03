package io.cyrene.tck

import com.google.protobuf.Any
import io.cyrene.proto.capability.v1.InvokeCapabilityRequest
import io.cyrene.proto.capability.v1.invokeCapabilityRequest
import io.cyrene.proto.model.provider.v1.EmbeddingBatch
import io.cyrene.proto.model.provider.v1.EmbeddingVector
import io.cyrene.proto.model.provider.v1.EmbeddingsRequest
import io.cyrene.proto.model.provider.v1.EmbeddingsResponse
import io.cyrene.proto.model.provider.v1.embeddingBatch
import io.cyrene.proto.model.provider.v1.embeddingVector
import io.cyrene.proto.model.provider.v1.embeddingsRequest
import io.cyrene.proto.model.provider.v1.embeddingsResponse
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class ModelProviderEmbeddingContractTck {
    @Test
    fun generatedJavaAndKotlinApisPreserveAnyAndOptionalBinding() {
        val embeddingRequest: EmbeddingsRequest = embeddingsRequest {
            inputs += listOf("alpha", "beta")
            model = "deterministic-model"
        }
        val oldRequest: InvokeCapabilityRequest = invokeCapabilityRequest {
            capability = "model.provider.v1"
            interfaceVersion = "1"
            method = "embeddings"
            request = Any.pack(embeddingRequest)
        }
        val oldRoundTrip = InvokeCapabilityRequest.parseFrom(oldRequest.toByteArray())
        assertFalse(oldRoundTrip.hasBindingId())

        val targeted = oldRequest.toBuilder().setBindingId("openai-main").build()
        val targetedRoundTrip = InvokeCapabilityRequest.parseFrom(targeted.toByteArray())
        assertTrue(targetedRoundTrip.hasBindingId())
        assertEquals("openai-main", targetedRoundTrip.bindingId)
        assertTrue(targetedRoundTrip.request.`is`(EmbeddingsRequest::class.java))

        val batch: EmbeddingBatch = embeddingBatch {
            vectors += listOf<EmbeddingVector>(
                embeddingVector { values += listOf(1.0f, 2.0f) },
                embeddingVector { values += listOf(3.0f, 4.0f) },
            )
            dimensions = 2
            model = "deterministic-model"
        }
        val response: EmbeddingsResponse = embeddingsResponse { embeddings = batch }
        val packed = Any.pack(response)
        assertTrue(packed.`is`(EmbeddingsResponse::class.java))
        assertEquals(embeddingRequest.inputsCount, packed.unpack(EmbeddingsResponse::class.java).embeddings.vectorsCount)
    }
}
