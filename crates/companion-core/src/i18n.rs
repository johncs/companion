//! GNU gettext localization initialization and translation helpers.

pub use gettextrs::{gettext, ngettext};

const GETTEXT_PACKAGE: &str = "companion";

/// Initialize locale and bind the `companion` gettext domain.
pub fn init() {
    let localedir = option_env!("COMPANION_LOCALEDIR").unwrap_or("/usr/share/locale");
    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    if let Err(err) = gettextrs::bindtextdomain(GETTEXT_PACKAGE, localedir) {
        tracing::warn!("Failed to bindtextdomain at {localedir}: {err}");
    }
    if let Err(err) = gettextrs::bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8") {
        tracing::warn!("Failed to bind_textdomain_codeset UTF-8: {err}");
    }
    if let Err(err) = gettextrs::textdomain(GETTEXT_PACKAGE) {
        tracing::warn!("Failed to set textdomain {GETTEXT_PACKAGE}: {err}");
    }
}

/// Translate `msgid` and substitute `{key}` placeholders in a single pass so translators
/// can reorder arguments without replacement values being re-expanded.
pub fn tr_f(msgid: &str, args: &[(&str, &str)]) -> String {
    crate::render_placeholders(&gettext(msgid), args)
}
