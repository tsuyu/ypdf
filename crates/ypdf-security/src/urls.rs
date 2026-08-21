//! Classifying URLs found in a document.
//!
//! No network access, ever — that would turn opening a report into visiting
//! every link in a hostile file. Everything here is decided from the text of
//! the URL alone.

/// What kind of URL this is, and how much it should worry someone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlKind {
    /// `https://` — ordinary.
    Secure,
    /// `http://` — ordinary, but unencrypted.
    Insecure,
    /// `mailto:` — an email link.
    Mail,
    /// `javascript:` — script in a link. Never ordinary.
    Script,
    /// `file:` — points into the reader's own filesystem.
    LocalFile,
    /// Points at a bare IP address rather than a name.
    RawAddress,
    /// Contains a punycode label, which can spell a familiar name in
    /// lookalike characters.
    Punycode,
    /// Some other scheme.
    Other,
}

impl UrlKind {
    /// How much a URL of this kind matters in a report.
    #[must_use]
    pub const fn severity(self) -> ypdf_doc::Severity {
        use ypdf_doc::Severity as S;
        match self {
            Self::Secure | Self::Mail => S::Info,
            Self::Insecure | Self::Other => S::Info,
            Self::RawAddress | Self::Punycode => S::Warning,
            Self::LocalFile => S::High,
            Self::Script => S::Critical,
        }
    }

    /// A short explanation for the report.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Secure => "https link",
            Self::Insecure => "unencrypted http link",
            Self::Mail => "email link",
            Self::Script => "javascript: link — script embedded in a link target",
            Self::LocalFile => "file: link — points at the reader's own filesystem",
            Self::RawAddress => "link to a bare IP address rather than a hostname",
            Self::Punycode => "link with a punycode hostname, which can imitate another name",
            Self::Other => "link with an uncommon scheme",
        }
    }
}

/// Classify a URL from its text alone.
#[must_use]
pub fn classify_url(url: &str) -> UrlKind {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();

    if lower.starts_with("javascript:") {
        return UrlKind::Script;
    }
    if lower.starts_with("file:") || lower.starts_with("\\\\") {
        return UrlKind::LocalFile;
    }
    if lower.starts_with("mailto:") {
        return UrlKind::Mail;
    }

    let host = host_of(&lower);
    if host.split('.').any(|label| label.starts_with("xn--")) {
        return UrlKind::Punycode;
    }
    if is_ip_literal(host) {
        return UrlKind::RawAddress;
    }

    if lower.starts_with("https://") {
        UrlKind::Secure
    } else if lower.starts_with("http://") {
        UrlKind::Insecure
    } else {
        UrlKind::Other
    }
}

/// The host part of a URL, or the whole thing when there is no scheme.
fn host_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    // Strip any userinfo, which is where a lookalike host usually hides:
    // `https://www.bank.com@192.0.2.1/`.
    let after_userinfo = after_scheme
        .rsplit_once('@')
        .map_or(after_scheme, |(_, rest)| rest);
    let host = after_userinfo
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_userinfo);
    // An IPv6 literal is bracketed and full of colons, so the port cannot be
    // stripped by splitting on the first one.
    if host.starts_with('[') {
        return host.split_once(']').map_or(host, |(inside, _)| inside);
    }
    host.split(':').next().unwrap_or(host)
}

fn is_ip_literal(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.contains(':') && host.chars().all(|c| c.is_ascii_hexdigit() || c == ':') {
        return true; // IPv6
    }
    let octets: Vec<&str> = host.split('.').collect();
    octets.len() == 4
        && octets
            .iter()
            .all(|o| !o.is_empty() && o.parse::<u8>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ypdf_doc::Severity;

    #[test]
    fn ordinary_links_are_informational() {
        assert_eq!(classify_url("https://example.org/docs"), UrlKind::Secure);
        assert_eq!(classify_url("http://example.org"), UrlKind::Insecure);
        assert_eq!(classify_url("mailto:someone@example.org"), UrlKind::Mail);
        assert_eq!(UrlKind::Secure.severity(), Severity::Info);
    }

    #[test]
    fn script_and_local_file_links_are_not_ordinary() {
        assert_eq!(classify_url("javascript:alert(1)"), UrlKind::Script);
        assert_eq!(classify_url("JavaScript:void(0)"), UrlKind::Script);
        assert_eq!(
            classify_url("file:///C:/Windows/System32/"),
            UrlKind::LocalFile
        );
        assert_eq!(
            classify_url(r"\\server\share\payload.exe"),
            UrlKind::LocalFile
        );

        assert_eq!(UrlKind::Script.severity(), Severity::Critical);
        assert_eq!(UrlKind::LocalFile.severity(), Severity::High);
    }

    #[test]
    fn bare_addresses_and_punycode_are_flagged() {
        assert_eq!(
            classify_url("http://192.0.2.10/invoice"),
            UrlKind::RawAddress
        );
        assert_eq!(classify_url("https://[2001:db8::1]/x"), UrlKind::RawAddress);
        assert_eq!(
            classify_url("https://xn--80ak6aa92e.com"),
            UrlKind::Punycode
        );
        assert_eq!(UrlKind::RawAddress.severity(), Severity::Warning);
    }

    #[test]
    fn a_lookalike_host_in_userinfo_does_not_hide_the_real_one() {
        // The real host here is the IP address, not the bank.
        assert_eq!(
            classify_url("https://www.bank.example@192.0.2.1/login"),
            UrlKind::RawAddress
        );
    }

    #[test]
    fn ports_and_paths_do_not_confuse_the_host() {
        assert_eq!(host_of("https://example.org:8443/a/b?c=d"), "example.org");
        assert_eq!(classify_url("https://example.org:8443/a"), UrlKind::Secure);
    }

    #[test]
    fn version_numbers_are_not_ip_addresses() {
        assert!(!is_ip_literal("1.2.3"));
        assert!(
            !is_ip_literal("999.1.1.1"),
            "an octet over 255 is not an address"
        );
        assert!(is_ip_literal("10.0.0.1"));
    }
}
