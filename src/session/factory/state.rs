//! Session Creation State Machine
//!
//! Formalizes the session creation process as an explicit state machine,
//! making retry logic, resource management, and error handling clear.

use std::sync::Arc;

use crate::portal::PortalManager;
use crate::session::strategy::SessionHandle;

/// Failure reasons during session creation
#[derive(Debug, Clone)]
pub enum SessionCreationFailure {
    /// Portal rejected persistence request (GNOME)
    PersistenceRejected { error_message: String },

    /// Source selection failed (user cancelled or other error)
    SourceSelectionFailed { error_message: String },

    /// Device selection failed
    DeviceSelectionFailed { error_message: String },

    /// Clipboard request failed
    ClipboardRequestFailed { error_message: String },

    /// Session start failed
    SessionStartFailed { error_message: String },

    /// Generic error
    Other { error_message: String },
}

impl SessionCreationFailure {
    pub fn should_retry_without_persistence(&self) -> bool {
        matches!(self, Self::PersistenceRejected { .. })
    }

    /// If persistence was rejected, clipboard was never used
    /// (select_sources failed before clipboard.request was called)
    pub fn should_reuse_clipboard(&self) -> bool {
        // If persistence was rejected, clipboard was never used
        // (select_sources failed before clipboard.request was called)
        matches!(self, Self::PersistenceRejected { .. })
    }

    pub fn message(&self) -> &str {
        match self {
            Self::PersistenceRejected { error_message } => error_message,
            Self::SourceSelectionFailed { error_message } => error_message,
            Self::DeviceSelectionFailed { error_message } => error_message,
            Self::ClipboardRequestFailed { error_message } => error_message,
            Self::SessionStartFailed { error_message } => error_message,
            Self::Other { error_message } => error_message,
        }
    }
}

/// Final error when session creation fails completely
#[derive(Debug, Clone)]
pub struct SessionCreationError {
    pub message: String,
    pub failures: Vec<SessionCreationFailure>,
}

impl std::fmt::Display for SessionCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Session creation failed: {}", self.message)
    }
}

impl std::error::Error for SessionCreationError {}

/// Session creation state machine
///
/// Tracks the current state of session creation, including resources
/// that should be preserved across retries.
pub enum SessionCreationState {
    /// Initial state - nothing allocated yet
    Init,

    /// Resources allocated, ready to attempt session creation
    ResourcesAllocated {
        portal_manager: Arc<PortalManager>,
        clipboard_manager: Option<Arc<lamco_portal::ClipboardManager>>,
    },

    /// First attempt in progress (not directly observable - transitions immediately)
    FirstAttempt {
        portal_manager: Arc<PortalManager>,
        clipboard_manager: Option<Arc<lamco_portal::ClipboardManager>>,
    },

    /// First attempt failed, preparing retry
    RetryPending {
        failure: SessionCreationFailure,
        /// Clipboard manager to reuse (if SingleClipboardProxy quirk applies)
        reusable_clipboard: Option<Arc<lamco_portal::ClipboardManager>>,
        attempt_number: u32,
    },

    /// Retry attempt in progress
    RetryAttempt {
        portal_manager: Arc<PortalManager>,
        clipboard_manager: Option<Arc<lamco_portal::ClipboardManager>>,
        attempt_number: u32,
    },

    /// Session created successfully
    Complete { session: Arc<dyn SessionHandle> },

    /// Terminal failure - no more retries
    Failed { error: SessionCreationError },
}

impl SessionCreationState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete { .. } | Self::Failed { .. })
    }

    pub fn attempt_number(&self) -> u32 {
        match self {
            Self::Init | Self::ResourcesAllocated { .. } | Self::FirstAttempt { .. } => 0,
            Self::RetryPending { attempt_number, .. }
            | Self::RetryAttempt { attempt_number, .. } => *attempt_number,
            Self::Complete { .. } | Self::Failed { .. } => 0, // N/A
        }
    }

    /// Extract clipboard manager if available (for reuse)
    pub fn take_clipboard(&mut self) -> Option<Arc<lamco_portal::ClipboardManager>> {
        match self {
            Self::ResourcesAllocated {
                clipboard_manager, ..
            } => clipboard_manager.take(),
            Self::FirstAttempt {
                clipboard_manager, ..
            } => clipboard_manager.take(),
            Self::RetryPending {
                reusable_clipboard, ..
            } => reusable_clipboard.take(),
            Self::RetryAttempt {
                clipboard_manager, ..
            } => clipboard_manager.take(),
            _ => None,
        }
    }
}

/// State transition result
pub enum StateTransition {
    /// Transition to a new state
    Transition(SessionCreationState),
    /// Stay in current state (shouldn't happen in normal flow)
    Stay,
}

// Manual Debug implementation because some fields don't implement Debug
impl std::fmt::Debug for SessionCreationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Init => write!(f, "Init"),
            Self::ResourcesAllocated { .. } => write!(f, "ResourcesAllocated"),
            Self::FirstAttempt { .. } => write!(f, "FirstAttempt"),
            Self::RetryPending { attempt_number, .. } => {
                write!(f, "RetryPending(attempt={})", attempt_number)
            }
            Self::RetryAttempt { attempt_number, .. } => {
                write!(f, "RetryAttempt(attempt={})", attempt_number)
            }
            Self::Complete { .. } => write!(f, "Complete"),
            Self::Failed { error } => write!(f, "Failed({})", error.message),
        }
    }
}
