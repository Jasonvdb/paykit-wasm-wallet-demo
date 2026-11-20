//! [`Failure`]: why an engine call failed, as text the page can show.
//!
//! The text crosses to JavaScript and is shown on the page and in the console, so it says
//! which step failed and the *kind* of error only: the variant name of the SDK's error and
//! the switch's error code when there is one. It never formats an SDK error's own text, which
//! can quote data such as a record or a message, and so might show more than a log should.

use std::fmt;

use paykit_sdk::PaykitError;

/// An engine call failed at a step. Shown to the user as `<step>: <kind>`.
#[derive(Debug)]
pub struct Failure(String);

impl Failure {
    /// A failure described by `text` alone, for checks the engine makes itself.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The SDK failed at `step`.
    pub fn sdk(step: &str, error: &PaykitError) -> Self {
        Self(format!("{step}: {}", sdk_kind(error)))
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The variant of `error`, plus the switch's error code when the switch answered with one.
fn sdk_kind(error: &PaykitError) -> String {
    // `PaykitError` is `#[non_exhaustive]`: a newer SDK may add variants, so a match outside
    // the SDK's crate must have a catch-all arm.
    let variant = match error {
        PaykitError::Client(_) => "Switch client error",
        PaykitError::Lib(_) => "Paykit rule error",
        PaykitError::Rail(_) => "payment rail error",
        PaykitError::Homeserver(_) => "Homeserver error",
        PaykitError::Refused(_) => "refused by a Paykit rule",
        PaykitError::Conflict(_) => "conflict",
        _ => "SDK error",
    };
    match error.code() {
        Some(code) => format!("{variant} ({})", code.as_str()),
        None => variant.to_owned(),
    }
}
