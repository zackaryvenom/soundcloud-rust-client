use adblock::{
    lists::{FilterSet, ParseOptions},
    request::Request,
    Engine,
};

const LOCAL_FILTERS: &str = r#"
||ads.soundcloud.com^
||ads.scdn.co^
||doubleclick.net^
||googlesyndication.com^
||googleadservices.com^
||adservice.google.com^
||amazon-adsystem.com^
||adnxs.com^
||adsrvr.org^
||criteo.com^
||taboola.com^
||outbrain.com^
||api.soundcloud.com/ads^
||api-v2.soundcloud.com/ads^
||delivery-cdn-cf.adswizz.com/soundcloud_newEnco/ad_*.mp3$media
"#;

pub fn build_filter_engine(filter_lists: impl IntoIterator<Item = String>) -> Engine {
    let mut filter_set = FilterSet::new(false);
    filter_set.add_filter_list(LOCAL_FILTERS.to_owned(), ParseOptions::default());
    for list in filter_lists {
        filter_set.add_filter_list(list, ParseOptions::default());
    }
    Engine::new_with_filter_set(filter_set)
}

pub fn should_block(engine: &Engine, raw_url: &str, source_url: &str, request_type: &str) -> bool {
    let Ok(request) = Request::new(raw_url, source_url, request_type, "GET") else {
        return false;
    };
    engine.check_network_request(&request).should_block()
}

#[cfg(test)]
mod tests {
    use super::{build_filter_engine, should_block};

    const SOURCE: &str = "https://soundcloud.com/";

    #[test]
    fn blocks_known_soundcloud_ad_audio_but_not_other_audio() {
        let engine = build_filter_engine([]);

        assert!(should_block(
            &engine,
            "https://delivery-cdn-cf.adswizz.com/soundcloud_newEnco/ad_test.mp3",
            SOURCE,
            "media"
        ));
        assert!(!should_block(
            &engine,
            "https://delivery-cdn-cf.adswizz.com/soundcloud_newEnco/track.mp3",
            SOURCE,
            "media"
        ));
    }

    #[test]
    fn applies_filter_lists_and_their_exceptions() {
        let engine = build_filter_engine([String::from(
            "||example.test^$media\n@@||example.test/allowed.mp3$media",
        )]);

        assert!(should_block(
            &engine,
            "https://example.test/ad.mp3",
            SOURCE,
            "media"
        ));
        assert!(!should_block(
            &engine,
            "https://example.test/allowed.mp3",
            SOURCE,
            "media"
        ));
    }

    #[test]
    fn matches_supported_ad_domains_and_soundcloud_ad_api() {
        let engine = build_filter_engine([]);

        assert!(should_block(
            &engine,
            "https://cdn.ads.scdn.co/banner",
            SOURCE,
            "image"
        ));
        assert!(should_block(
            &engine,
            "https://api-v2.soundcloud.com/ads",
            SOURCE,
            "fetch"
        ));
        assert!(!should_block(
            &engine,
            "https://api-v2.soundcloud.com/users/ads",
            SOURCE,
            "fetch"
        ));
    }
}
