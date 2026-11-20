use url::Url;

const AD_DOMAINS: &[&str] = &[
    "ads.soundcloud.com",
    "ads.scdn.co",
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "adservice.google.com",
    "amazon-adsystem.com",
    "adnxs.com",
    "adsrvr.org",
    "criteo.com",
    "taboola.com",
    "outbrain.com",
];

pub fn blocked_reason(raw_url: &str) -> Option<&'static str> {
    let url = Url::parse(raw_url).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }

    let host = url.host_str()?;
    if AD_DOMAINS.iter().any(|domain| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    }) {
        return Some("known advertising domain");
    }

    if matches!(host, "api.soundcloud.com" | "api-v2.soundcloud.com")
        && matches!(url.path(), "/ads" | "/ads/")
    {
        return Some("SoundCloud ad API");
    }

    None
}

#[cfg(test)]
mod tests {
    use super::blocked_reason;

    #[test]
    fn blocks_soundcloud_ad_domains_and_subdomains() {
        assert_eq!(
            blocked_reason("https://ads.soundcloud.com/slot"),
            Some("known advertising domain")
        );
        assert_eq!(
            blocked_reason("https://cdn.ads.scdn.co/banner"),
            Some("known advertising domain")
        );
    }

    #[test]
    fn blocks_ad_domain_suffixes_without_matching_lookalikes() {
        assert_eq!(
            blocked_reason("https://pagead2.googlesyndication.com/impression"),
            Some("known advertising domain")
        );
        assert_eq!(blocked_reason("https://notdoubleclick.net/ad"), None);
        assert_eq!(
            blocked_reason("https://doubleclick.net.attacker.example/ad"),
            None
        );
    }

    #[test]
    fn blocks_only_soundcloud_ad_api_paths() {
        assert_eq!(
            blocked_reason("https://api-v2.soundcloud.com/ads"),
            Some("SoundCloud ad API")
        );
        assert_eq!(
            blocked_reason("https://api.soundcloud.com/ads/"),
            Some("SoundCloud ad API")
        );
        assert_eq!(
            blocked_reason("https://api-v2.soundcloud.com/users/ads"),
            None
        );
        assert_eq!(
            blocked_reason("https://api-v2.soundcloud.com/ads/list"),
            None
        );
    }

    #[test]
    fn ignores_invalid_and_non_web_urls() {
        assert_eq!(blocked_reason("not a URL"), None);
        assert_eq!(blocked_reason("file:///ads.soundcloud.com/slot"), None);
    }

    #[test]
    fn recognizes_the_manual_blocking_demo_request() {
        assert!(blocked_reason("https://ads.soundcloud.com/rustcloud-block-test.gif").is_some());
    }
}
