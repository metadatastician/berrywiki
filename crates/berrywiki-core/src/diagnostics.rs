//! Structured, non-fatal diagnostics.
//!
//! The engine never panics on malformed input. Instead it degrades gracefully
//! and records a [`Diagnostic`] so the UI can surface an actionable warning
//! while the page still opens. (Non-negotiable requirements 9, 10 and the
//! metadata rule "malformed metadata must not make a page unreadable".)

use std::fmt;

/// Severity of a diagnostic. Ordered so `Error > Warning > Info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        f.write_str(s)
    }
}

/// A single actionable diagnostic tied, where possible, to a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    /// Stable identifier for the *kind* of problem (e.g. `metadata.bad-position`).
    /// Machine-readable; safe to match on in tests and to route in a UI.
    pub code: String,
    /// Human-readable, actionable message. Must never contain secrets or paths
    /// outside the wiki.
    pub message: String,
    /// Page this diagnostic relates to, if known (page id or path).
    pub page: Option<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, code: impl Into<String>, message: impl Into<String>) -> Self {
        Diagnostic {
            severity,
            code: code.into(),
            message: message.into(),
            page: None,
        }
    }

    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Diagnostic::new(Severity::Warning, code, message)
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Diagnostic::new(Severity::Error, code, message)
    }

    #[must_use]
    pub fn with_page(mut self, page: impl Into<String>) -> Self {
        self.page = Some(page.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.page {
            Some(p) => write!(f, "[{}] {} ({}): {}", self.severity, self.code, p, self.message),
            None => write!(f, "[{}] {} : {}", self.severity, self.code, self.message),
        }
    }
}
