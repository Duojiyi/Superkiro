//! Bidirectional request and response translator between Kiro IDE and model providers.
//!
//! Spec §4.3:
//! - Conversation state translation to Provider ChatRequest.
//! - Provider stream events translation to AWS EventStream binary frames.
//! - Tool name shortening and restoration.
//! - Tool documentation relocation.
//! - Image compression and resizing.
//! - Orphan tool call / tool result pair repair.

pub mod from_provider;
pub mod images;
pub mod to_provider;
pub mod tools;
pub mod vision;

pub use from_provider::{translate_provider_event_to_frames, StreamTranslationState};
pub use images::{
    maybe_shrink_image, validate_image_bytes, validate_image_input, ImageValidationError, Omission,
    PreparedImage,
};
pub use to_provider::{
    prepare_images, translate_kiro_to_chat_request, PreparedImages, TranslationContext,
};
pub use tools::{process_tools_for_provider, repair_orphan_tool_pairs, ToolRegistry};
pub use vision::{
    format_fallback_description, model_supports_vision, transcribe_image_with_provider,
    VisionFallbackCache, VisionFallbackConfig,
};
