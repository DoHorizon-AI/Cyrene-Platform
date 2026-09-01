//! The narrow, product-neutral `media.processor.v1` contract.
//!
//! This module contains data types and lifecycle/error semantics only.  It does
//! not read files, decode media, persist attachments, or make Product policy
//! decisions.  A concrete implementation may be hosted inline or by a worker;
//! the resolver's execution-mode contract remains transport-neutral.

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt::{Display, Formatter};

/// Canonical capability identifier for the first media-processing slice.
pub const MEDIA_PROCESSOR_V1: &str = "media.processor.v1";
/// Callable interface version, independent of an implementation release.
pub const MEDIA_PROCESSOR_INTERFACE_V1: &str = "1";
/// First operation in the contract.
pub const INSPECT_IMAGE_OPERATION: &str = "inspect_image";
/// Second operation in the contract.
pub const TRANSFORM_IMAGE_OPERATION: &str = "transform_image";
/// Narrow audio normalization operation used by Product record ingestion.
pub const NORMALIZE_AUDIO_OPERATION: &str = "normalize_audio";

/// Image formats that the contract can name without binding callers to a
/// particular codec implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
    Bmp,
    Tiff,
}

impl ImageFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Gif => "gif",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
        }
    }
}

impl Display for ImageFormat {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// EXIF/TIFF orientation values, kept numeric for wire compatibility with
/// image libraries while constraining the value to the standard 1..=8 range.
pub type ImageOrientation = u8;

/// Explicit caller-provided media reference.
///
/// `Bytes` is base64 only because this is a JSON-safe contract representation;
/// it is not a free-form string carrying an alternate source kind.  `File`
/// names a caller-owned path.  URLs, data URIs, streams, and opaque handles
/// are intentionally not part of this first slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageInput {
    Bytes {
        data_base64: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<ImageFormat>,
    },
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<ImageFormat>,
    },
}

/// JSON-safe typed image bytes returned by a transform operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedImage {
    pub data_base64: String,
    pub media_type: ImageFormat,
}

/// Generic facts observed by `inspect_image`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageInspection {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub orientation: ImageOrientation,
    pub size_bytes: u64,
    /// Lowercase hexadecimal SHA-256 of the caller-provided input bytes.
    pub content_sha256: String,
}

/// Optional resize operation.  A preserving resize fits within the requested
/// bounds; otherwise the implementation produces the exact dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeOptions {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub preserve_aspect_ratio: bool,
}

impl ResizeOptions {
    pub fn validate(self) -> Result<(), MediaProcessorError> {
        if self.width == 0 || self.height == 0 {
            return Err(MediaProcessorError::invalid_input(
                "resize width and height must be positive",
            ));
        }
        Ok(())
    }
}

/// Typed request for `inspect_image`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectImageRequest {
    pub input: ImageInput,
}

/// Typed request for `transform_image`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransformImageRequest {
    pub input: ImageInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resize: Option<ResizeOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<ImageFormat>,
    /// Codec quality from 1 (lowest) through 100 (highest), where supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
    #[serde(default)]
    pub normalize_orientation: bool,
}

impl TransformImageRequest {
    pub fn validate(&self) -> Result<(), MediaProcessorError> {
        if let Some(resize) = self.resize {
            resize.validate()?;
        }
        if let Some(quality) = self.quality {
            if !(1..=100).contains(&quality) {
                return Err(MediaProcessorError::invalid_input(
                    "quality must be between 1 and 100",
                ));
            }
        }
        Ok(())
    }
}

/// Result returned by `transform_image`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransformedImage {
    pub content: EncodedImage,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub orientation: ImageOrientation,
    pub size_bytes: u64,
    /// Lowercase hexadecimal SHA-256 of the returned encoded bytes.
    pub content_sha256: String,
}

/// Explicit caller-provided audio reference. Remote acquisition and attachment
/// authorization remain Product responsibilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioInput {
    Bytes {
        data_base64: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
    },
    File {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
    },
}

/// The first bounded Product-selected canonical audio profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalAudioProfile {
    WavPcmS16leMono16000,
}

/// Typed request for `normalize_audio`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizeAudioRequest {
    pub input: AudioInput,
    pub target_profile: CanonicalAudioProfile,
}

/// JSON-safe normalized audio bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedAudio {
    pub data_base64: String,
    pub media_type: String,
}

/// Result returned by `normalize_audio`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedAudio {
    pub content: EncodedAudio,
    pub format: String,
    pub codec: String,
    pub profile: CanonicalAudioProfile,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub duration_ms: u64,
    pub size_bytes: u64,
    pub content_sha256: String,
}

pub use crate::worker::{AtomicCancellationToken, CancellationToken, NeverCancelled};

/// Stable, transport-neutral error categories for the first slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaProcessorError {
    InvalidInput(String),
    UnsupportedInput(String),
    Cancelled,
    ExecutionFailed(String),
}

impl MediaProcessorError {
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput(message.into())
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "INVALID_INPUT",
            Self::UnsupportedInput(_) => "UNSUPPORTED_INPUT",
            Self::Cancelled => "CANCELLED",
            Self::ExecutionFailed(_) => "EXECUTION_FAILED",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::InvalidInput(message)
            | Self::UnsupportedInput(message)
            | Self::ExecutionFailed(message) => message,
            Self::Cancelled => "operation cancelled",
        }
    }
}

impl Display for MediaProcessorError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message())
    }
}

impl Error for MediaProcessorError {}

/// Platform-owned capability port implemented by an official or third-party
/// generic media plugin.
pub trait MediaProcessor {
    fn inspect_image(
        &self,
        request: &InspectImageRequest,
        cancellation: &dyn CancellationToken,
    ) -> Result<ImageInspection, MediaProcessorError>;

    fn transform_image(
        &self,
        request: &TransformImageRequest,
        cancellation: &dyn CancellationToken,
    ) -> Result<TransformedImage, MediaProcessorError>;

    fn normalize_audio(
        &self,
        request: &NormalizeAudioRequest,
        cancellation: &dyn CancellationToken,
    ) -> Result<NormalizedAudio, MediaProcessorError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_model_keeps_bytes_and_file_references_explicit() {
        let bytes = serde_json::to_value(ImageInput::Bytes {
            data_base64: "AAEC".to_string(),
            media_type: Some(ImageFormat::Png),
        })
        .unwrap();
        assert_eq!(bytes["kind"], "bytes");
        assert!(bytes.get("data_base64").is_some());
        assert!(bytes.get("path").is_none());

        let file = serde_json::to_value(ImageInput::File {
            path: "fixture.png".to_string(),
            media_type: None,
        })
        .unwrap();
        assert_eq!(file["kind"], "file");
        assert!(file.get("path").is_some());
        assert!(file.get("data_base64").is_none());
    }

    #[test]
    fn transform_validation_rejects_invalid_ranges() {
        let request = TransformImageRequest {
            input: ImageInput::Bytes {
                data_base64: "AAEC".to_string(),
                media_type: None,
            },
            resize: Some(ResizeOptions {
                width: 0,
                height: 2,
                preserve_aspect_ratio: false,
            }),
            output_format: None,
            quality: Some(101),
            normalize_orientation: false,
        };
        assert_eq!(request.validate().unwrap_err().code(), "INVALID_INPUT");
    }

    #[test]
    fn error_codes_are_transport_neutral() {
        assert_eq!(MediaProcessorError::Cancelled.code(), "CANCELLED");
        assert_eq!(
            MediaProcessorError::UnsupportedInput("ppm".to_string()).code(),
            "UNSUPPORTED_INPUT"
        );
    }

    #[test]
    fn audio_contract_has_one_bounded_canonical_profile() {
        let request = NormalizeAudioRequest {
            input: AudioInput::Bytes {
                data_base64: "UklGRg==".to_string(),
                media_type: Some("audio/wav".to_string()),
            },
            target_profile: CanonicalAudioProfile::WavPcmS16leMono16000,
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["target_profile"], "wav_pcm_s16le_mono16000");
        assert_eq!(value["input"]["kind"], "bytes");
    }
}
