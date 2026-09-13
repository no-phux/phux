#!/usr/bin/env python3
"""Instrument safety tests, independent from the live product contract assertions."""

import importlib.util
import os
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("smoke", Path(__file__).with_name("everyday-ux-smoke.py"))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class InstrumentTests(unittest.TestCase):
    def test_sibling_app_invalidates_serial_ownership(self):
        with patch.object(smoke.subprocess, "run", side_effect=[Mock(stdout="123\n"), Mock(stdout="456\n")]):
            with self.assertRaisesRegex(smoke.InfrastructureError, "ownership changed"):
                smoke.ensure_serial(123)

    def test_menu_and_hidden_source_do_not_satisfy_rendered_ui(self):
        text = ('menu item name="Machines"\n'
                'source role=button name="Machines"\n'
                '    widget @w1/canvas#42 role=button name="Use this Mac" bounds=(0,0 30x40) focused=false enabled=true\n')
        self.assertFalse(smoke.named(text, "Machines"))
        self.assertFalse(smoke.named(text, "Mac"))
        self.assertTrue(smoke.named(text, "Use this Mac"))

    def test_environment_cannot_inherit_remote_or_config_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.dict(os.environ, {"PHUX_REMOTE": "real-host", "PHUX_PROFILE": "real-profile",
                                         "PHUX_COCKPIT_CONFIG": "/real/config", "XDG_CONFIG_HOME": "/real",
                                         "NATIVE_SDK_GPU_COMPOSITE": "1"}):
                env = smoke.isolated_environment(Path(directory))
            for key in ("PHUX_REMOTE", "PHUX_PROFILE", "NATIVE_SDK_GPU_COMPOSITE"):
                self.assertNotIn(key, env)
            for key in ("PHUX_SOCKET", "PHUX_COCKPIT_CONFIG", "PHUX_COCKPIT_STATE", "HOME", "XDG_CONFIG_HOME"):
                self.assertTrue(Path(env[key]).is_relative_to(directory))

    def test_stale_publisher_refuses_input(self):
        with tempfile.TemporaryDirectory() as directory:
            journey = smoke.Journey(Path(directory), "/native", "/phux")
            journey.app = Mock(pid=123)
            journey.app.poll.return_value = None
            journey.applescript = Mock()
            with patch.object(smoke, "run", return_value="ready=true publisher_pid=456"), patch.object(smoke, "ensure_serial"):
                with self.assertRaisesRegex(smoke.InfrastructureError, "publisher changed"):
                    journey.input('keystroke "x"')
            journey.applescript.assert_not_called()

    def test_wrong_frontmost_refuses_input(self):
        with tempfile.TemporaryDirectory() as directory:
            journey = smoke.Journey(Path(directory), "/native", "/phux")
            journey.app = Mock(pid=123)
            journey.snapshot = Mock()
            journey.frontmost = Mock(return_value="456")
            journey.applescript = Mock()
            with self.assertRaisesRegex(smoke.InfrastructureError, "focus changed"):
                journey.input('keystroke "x"')
            journey.applescript.assert_not_called()

    def test_pointer_lookup_dereferences_entire_contents_items(self):
        with tempfile.TemporaryDirectory() as directory:
            journey = smoke.Journey(Path(directory), "/native", "/phux")
            journey.app = Mock(pid=123)
            journey.snapshot = Mock()
            journey.frontmost = Mock(return_value="123")
            journey.applescript = Mock(return_value="1132,195,206,139,1100,640")
            journey.pointer_click = Mock()

            self.assertTrue(journey.click_named("Machines"))
            script = journey.applescript.call_args.args[0]
            self.assertIn("set candidate to contents of elementRef", script)
            self.assertIn('description of candidate is "Machines"', script)
            self.assertIn("on error\nend try", script)
            journey.pointer_click.assert_called_once_with("1132,195")

    def test_pointer_lookup_refuses_offscreen_accessibility_target(self):
        with tempfile.TemporaryDirectory() as directory:
            journey = smoke.Journey(Path(directory), "/native", "/phux")
            journey.app = Mock(pid=123)
            journey.snapshot = Mock()
            journey.frontmost = Mock(return_value="123")
            journey.applescript = Mock(return_value="592,1078,230,115,1100,640")
            journey.pointer_click = Mock()

            self.assertFalse(journey.click_named("Edit Configuration"))
            journey.pointer_click.assert_not_called()

    def test_applescript_strings_preserve_unicode_menu_labels(self):
        self.assertEqual(smoke.applescript_string("Show All Windows…"), '"Show All Windows…"')

    def test_missing_publisher_is_infrastructure_failure(self):
        with self.assertRaises(smoke.InfrastructureError):
            smoke.publisher("ready=true\n")
        with self.assertRaises(smoke.InfrastructureError):
            smoke.publisher('ready=true\nwidget @w1/canvas#1 role=text name="publisher_pid=123"')

    def test_search_requires_usable_textbox_in_invoking_window(self):
        node = 'widget @w{window}/canvas#{identity} role={role} name="Search settings" bounds=(0,0 {width}x40) focused=false enabled={enabled}\n'
        invalid = [dict(window=1, role="text", width=30, enabled="true"),
                   dict(window=2, role="textbox", width=30, enabled="true"),
                   dict(window=1, role="textbox", width=0, enabled="true"),
                   dict(window=1, role="textbox", width=30, enabled="false")]
        text = 'window @w2 "Other window" focused=true\n'
        text += "".join(node.format(identity=index, **fields) for index, fields in enumerate(invalid))
        self.assertFalse(smoke.named(text, "Search settings", role="textbox", window=1))
        text += node.format(identity=99, window=1, role="textbox", width=30, enabled="true")
        self.assertTrue(smoke.named(text, "Search settings", role="textbox", window=1))

    def test_overview_rows_belong_to_the_distinctive_list(self):
        text = ('widget @w1/canvas#1 role=list name="Open windows" bounds=(0,0 100x100) focused=false enabled=true\n'
                'widget @w1/canvas#2 role=listitem name="Unrelated machine" bounds=(0,0 100x40) focused=false enabled=true parent=#99\n'
                'widget @w1/canvas#3 role=listitem name="Window A" bounds=(0,0 100x40) focused=false enabled=true parent=#1\n')
        self.assertEqual(smoke.list_rows(text, "Open windows"), 1)

    def test_foreign_build_environment_cannot_choose_linked_ffi(self):
        target = "aarch64-apple-darwin"
        with patch.dict(os.environ, {"CARGO_TARGET_DIR": "/foreign/target", "CARGO_BUILD_TARGET": "wasm32-wasip1", "PHUX_CLIENT_FFI_INCLUDE_DIR": "/foreign/include", "PHUX_CLIENT_FFI_LIB_DIR": "/foreign/lib"}):
            with tempfile.TemporaryDirectory() as directory, patch.object(smoke.subprocess, "run") as runner:
                smoke.build(Path(directory), target)
        cargo, zig = runner.call_args_list
        self.assertEqual(cargo.kwargs["env"]["CARGO_TARGET_DIR"], str(smoke.CHECKOUT / "target"))
        self.assertNotIn("PHUX_CLIENT_FFI_LIB_DIR", zig.kwargs["env"])
        self.assertNotIn("PHUX_CLIENT_FFI_INCLUDE_DIR", zig.kwargs["env"])
        self.assertNotIn("CARGO_BUILD_TARGET", cargo.kwargs["env"])
        self.assertNotIn("CARGO_BUILD_TARGET", zig.kwargs["env"])
        self.assertEqual(cargo.args[0][cargo.args[0].index("--target") + 1], target)
        self.assertIn(f"-Dphux-client-ffi-include-dir={smoke.FFI_HEADER.parents[1]}", zig.args[0])
        self.assertIn(f"-Dphux-client-ffi-lib-dir={smoke.ffi_archive(target).parent}", zig.args[0])

    def test_cargo_config_target_cannot_redirect_selected_host_archive(self):
        # Simulate foreign and same-host config defaults: both used to redirect
        # Cargo output while Zig continued consuming the legacy unqualified path.
        for configured in ("wasm32-wasip1", "aarch64-apple-darwin"):
            with self.subTest(configured=configured), tempfile.TemporaryDirectory() as directory:
                checkout = Path(directory)
                config = checkout / ".cargo/config.toml"
                config.parent.mkdir()
                config.write_text(f'[build]\ntarget = "{configured}"\n')
                with patch.object(smoke, "CHECKOUT", checkout), patch.object(smoke, "run", return_value="rustc fixture\nhost: aarch64-apple-darwin\n") as probe:
                    target = smoke.native_target()
                    with patch.object(smoke.subprocess, "run") as runner:
                        smoke.build(checkout, target)
                self.assertEqual(probe.call_args.args[0], ["rustc", "-vV"])
                cargo, zig = runner.call_args_list
                self.assertEqual(cargo.args[0][cargo.args[0].index("--target") + 1], target)
                expected = checkout / "target/aarch64-apple-darwin/ffi-dev"
                self.assertIn(f"-Dphux-client-ffi-lib-dir={expected}", zig.args[0])
                self.assertEqual(config.read_text(), f'[build]\ntarget = "{configured}"\n')

    def test_missing_rustc_host_is_not_guessed(self):
        with patch.object(smoke, "run", return_value="rustc fixture without host"):
            with self.assertRaisesRegex(smoke.InfrastructureError, "host target"):
                smoke.native_target()

    def test_build_environment_clears_target_override(self):
        with patch.dict(os.environ, {"CARGO_BUILD_TARGET": "wasm32-wasip1"}):
            self.assertIsNone(smoke.build_environment().get("CARGO_BUILD_TARGET"))

    def test_stubborn_app_cannot_skip_server_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            journey = smoke.Journey(Path(directory), "/native", "/phux")
            journey.app, journey.server = Mock(), Mock()
            journey.app.poll.return_value = journey.server.poll.return_value = None
            journey.app.wait.side_effect = [subprocess.TimeoutExpired("app", 15), None]
            handle = Mock()
            journey.handles = [handle]
            journey.close()
            journey.app.kill.assert_called_once()
            self.assertEqual(journey.app.wait.call_count, 2)
            journey.server.terminate.assert_called_once()
            handle.close.assert_called_once()

    def test_cleanup_failure_is_infrastructure_and_still_closes_other_children(self):
        with tempfile.TemporaryDirectory() as directory:
            journey = smoke.Journey(Path(directory), "/native", "/phux")
            journey.app, journey.server = Mock(), Mock()
            journey.app.poll.return_value = journey.server.poll.return_value = None
            journey.app.wait.side_effect = subprocess.TimeoutExpired("app", 15)
            handle = Mock()
            journey.handles = [handle]
            with self.assertRaises(smoke.InfrastructureError):
                journey.close()
            journey.server.terminate.assert_called_once()
            handle.close.assert_called_once()
            self.assertTrue((Path(directory) / "cleanup-error.txt").exists())


if __name__ == "__main__":
    unittest.main()
