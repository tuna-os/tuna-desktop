#!/usr/bin/env python3
"""Source contracts only; native worker/watch/clock assertions execute in Rust CI."""
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
READER = (ROOT / "crates/compositor/src/night_light_settings.rs").read_text()
MODEL = (ROOT / "crates/compositor/src/night_light.rs").read_text()
RUNTIME = (ROOT / "crates/compositor/src/runtime.rs").read_text()
BUS = (ROOT / "crates/compositor/src/mutter.rs").read_text()


class ReaderContracts(unittest.TestCase):
    def test_production_render_and_bus_have_no_settings_process(self):
        self.assertNotIn("load_from_gsettings", MODEL + RUNTIME + BUS)
        self.assertNotIn("std::process::Command", READER.split("#[cfg(test)]\nmod tests")[0])
        self.assertIn(".poll_settings(Instant::now())", RUNTIME)
        self.assertIn("night_light_share.start_reader(&runtime.wake)", RUNTIME)
        self.assertIn("async fn night_light_preview", BUS)
        self.assertIn("Ok(request) => request.await", BUS)

    def test_native_context_has_one_owner_and_no_inline_invoke(self):
        self.assertIn("context.with_thread_default", READER)
        self.assertIn("thread_local!", READER)
        self.assertIn("glib::idle_source_new", READER)
        self.assertNotRegex(READER, r"\.invoke\s*\(")
        self.assertEqual(READER.count('.name("tuna-night-light".into())'), 1)
        self.assertIn("local.creator != std::thread::current().id()", READER)
        drop = READER.split("impl Drop for Reader {", 1)[1].split("impl Shared {", 1)[0]
        self.assertNotRegex(drop, r"\.join\s*\(")

    def test_fixed_types_and_admission_failure_are_explicit(self):
        self.assertIn("Duration::from_secs(2)", READER)
        self.assertIn('const KEYS: [(&str, &str); 6]', READER)
        self.assertIn("schema.key(key).value_type().as_str() != signature", READER)
        self.assertIn("CacheStatus::Quarantined", READER)
        self.assertIn("state.stopped = true", READER)
        self.assertIn("state.active = None", READER)
        self.assertIn("state.preview.take()", READER)

    def test_preview_validates_before_reader_and_retains_request_expiry(self):
        method = MODEL.split("pub(crate) fn request_preview(", 1)[1].split("pub(crate) fn record_preview_until", 1)[0]
        self.assertLess(method.index("duration == 0"), method.index(".reader"))
        self.assertLess(method.index("let admitted"), method.index(".reader"))
        self.assertIn(".preview(duration, admitted)", method)
        self.assertIn("op.admitted >= request.admitted", READER)
        self.assertIn("record_preview_until(config.temperature_k(), request.until)", READER)
        self.assertIn("request.id == self.id", READER)
        self.assertIn("reader.cancel_preview()", MODEL)

    def test_signed_clock_clear_and_bus_notification_are_real_consumers(self):
        self.assertIn(".tick_at_system_time(SystemTime::now(), Instant::now())", RUNTIME)
        self.assertIn("self.share.clear_disabled_until_if(set_at)", MODEL)
        self.assertIn("if *set != Some(expected)", MODEL)
        self.assertIn("color.disabled_until_tomorrow_changed(interface.signal_emitter())", BUS)
        self.assertIn("if color_owned && disabled != last_disabled", BUS)
        self.assertIn("actual_gio_memory_watch_updates_typed_cache", READER)
        self.assertIn("actual_controller_failed_cache_keeps_config", READER)


if __name__ == "__main__":
    unittest.main()
