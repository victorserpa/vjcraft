//! Own process: the warm-up runs once per process, so no other test may start it first.

#[test]
fn enabling_the_pref_after_off_starts_the_warm_up() {
    vjc_media::configure_hw_decode(false, 8);
    assert!(!vjc_media::hw_warm_up_started());
    vjc_media::configure_hw_decode(true, 8);
    // Platforms without a hardware device never warm up.
    if cfg!(any(unix, windows)) {
        assert!(vjc_media::hw_warm_up_started());
    }
}
