//! The closed error vocabulary of the v2 `open` and `read` tools.
//!
//! The v1 tools keep their own `ApplicationError` mapping. The v2 tools speak only the codes of the
//! approved contract, so every failure passes through this type before it reaches the wire.

use crate::application::ports::ApplicationError;

/// The v2 error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingErrorCode {
    NotOpened,
    UnknownHandle,
    StaleHandle,
    TooLong,
    InvalidArgument,
    TooLarge,
    BlockedSource,
    FetchFailed,
    OcrRequired,
    Unreadable,
    Internal,
}

impl ReadingErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotOpened => "not_opened",
            Self::UnknownHandle => "unknown_handle",
            Self::StaleHandle => "stale_handle",
            Self::TooLong => "too_long",
            Self::InvalidArgument => "invalid_argument",
            Self::TooLarge => "too_large",
            Self::BlockedSource => "blocked_source",
            Self::FetchFailed => "fetch_failed",
            Self::OcrRequired => "ocr_required",
            Self::Unreadable => "unreadable",
            Self::Internal => "internal",
        }
    }
}

/// A v2 tool failure: a code, the field the caller must fix, and a short internal detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingError {
    pub code: ReadingErrorCode,
    /// The request field that caused an `invalid_argument`, when one field is at fault.
    pub field: Option<&'static str>,
    /// Internal detail. Never sent as-is; the tool layer turns it into a Chinese hint.
    pub detail: String,
    /// The character count of the first item, for `too_long`.
    pub chars: Option<usize>,
}

impl ReadingError {
    pub fn new(code: ReadingErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            field: None,
            detail: detail.into(),
            chars: None,
        }
    }

    pub fn invalid(field: &'static str, detail: impl Into<String>) -> Self {
        Self {
            field: Some(field),
            ..Self::new(ReadingErrorCode::InvalidArgument, detail)
        }
    }

    pub fn not_opened() -> Self {
        Self::new(
            ReadingErrorCode::NotOpened,
            "the document is not open in this server",
        )
    }

    pub fn unknown_handle(detail: impl Into<String>) -> Self {
        Self::new(ReadingErrorCode::UnknownHandle, detail)
    }

    pub fn stale_handle(detail: impl Into<String>) -> Self {
        Self::new(ReadingErrorCode::StaleHandle, detail)
    }

    pub fn too_long(chars: usize) -> Self {
        Self {
            chars: Some(chars),
            ..Self::new(
                ReadingErrorCode::TooLong,
                format!("the first item contains {chars} characters"),
            )
        }
    }
}

impl From<ApplicationError> for ReadingError {
    fn from(error: ApplicationError) -> Self {
        let detail = error.to_string();
        // No wildcard arm: a new `ApplicationError` variant must pick its v2 code here.
        let code = match &error {
            ApplicationError::DocumentNotFound => ReadingErrorCode::NotOpened,
            ApplicationError::SectionNotFound => ReadingErrorCode::UnknownHandle,
            ApplicationError::StaleDocument(_)
            | ApplicationError::StaleLocator(_)
            | ApplicationError::StaleStructure(_) => ReadingErrorCode::StaleHandle,
            ApplicationError::InvalidRequest(_)
            | ApplicationError::InvalidLocator(_)
            | ApplicationError::InvalidCursor(_)
            | ApplicationError::StaleCursor(_)
            | ApplicationError::CursorTargetMismatch(_)
            | ApplicationError::CursorEncodingFailed(_) => ReadingErrorCode::InvalidArgument,
            ApplicationError::ResourceLimitExceeded(_) | ApplicationError::OcrResourceLimit => {
                ReadingErrorCode::TooLarge
            }
            ApplicationError::BlockedSource(_) | ApplicationError::AuthenticationFailed(_) => {
                ReadingErrorCode::BlockedSource
            }
            ApplicationError::OcrRequired(_)
            | ApplicationError::OcrNoSupportedProjection
            | ApplicationError::OcrFailed
            | ApplicationError::OcrUnavailable => ReadingErrorCode::OcrRequired,
            ApplicationError::RetrievalFailed(_)
            | ApplicationError::OcrBusy
            | ApplicationError::OcrTimeout => ReadingErrorCode::FetchFailed,
            ApplicationError::ParseFailed(_) | ApplicationError::UnsupportedTextEncoding(_) => {
                ReadingErrorCode::Unreadable
            }
            ApplicationError::RepositoryFailed(_)
            | ApplicationError::CacheFailed(_)
            | ApplicationError::IndexFailed(_)
            | ApplicationError::TextUnitIndexFailed(_)
            | ApplicationError::SourceViewFailed(_) => ReadingErrorCode::Internal,
        };
        Self::new(code, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::ReadingErrorCode;

    #[test]
    fn codes_are_the_contract_strings() {
        assert_eq!(ReadingErrorCode::NotOpened.as_str(), "not_opened");
        assert_eq!(ReadingErrorCode::UnknownHandle.as_str(), "unknown_handle");
        assert_eq!(ReadingErrorCode::StaleHandle.as_str(), "stale_handle");
        assert_eq!(ReadingErrorCode::TooLong.as_str(), "too_long");
        assert_eq!(
            ReadingErrorCode::InvalidArgument.as_str(),
            "invalid_argument"
        );
        assert_eq!(ReadingErrorCode::TooLarge.as_str(), "too_large");
        assert_eq!(ReadingErrorCode::BlockedSource.as_str(), "blocked_source");
        assert_eq!(ReadingErrorCode::FetchFailed.as_str(), "fetch_failed");
        assert_eq!(ReadingErrorCode::OcrRequired.as_str(), "ocr_required");
        assert_eq!(ReadingErrorCode::Unreadable.as_str(), "unreadable");
        assert_eq!(ReadingErrorCode::Internal.as_str(), "internal");
    }
}
