//! GNU gettext localization initialization and translation helpers.

#[allow(unused_imports)]
pub use gettextrs::{gettext, ngettext};

const GETTEXT_PACKAGE: &str = "companion";

/// Initialize locale and bind the `companion` gettext domain.
pub fn init() {
    let localedir = option_env!("COMPANION_LOCALEDIR").unwrap_or("/usr/share/locale");
    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    if let Err(err) = gettextrs::bindtextdomain(GETTEXT_PACKAGE, localedir) {
        tracing::warn!("Failed to bindtextdomain at {localedir}: {err}");
    }
    let _ = gettextrs::bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8");
    if let Err(err) = gettextrs::textdomain(GETTEXT_PACKAGE) {
        tracing::warn!("Failed to set textdomain {GETTEXT_PACKAGE}: {err}");
    }
}

/// Translate `msgid` and substitute `{key}` placeholders so translators can reorder arguments.
pub fn tr_f(msgid: &str, args: &[(&str, &str)]) -> String {
    let mut out = gettext(msgid);
    for (key, val) in args {
        out = out.replace(&format!("{{{key}}}"), val);
    }
    out
}
