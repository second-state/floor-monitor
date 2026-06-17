import json
import unittest

from camera_client import (
    OnvifPtzController,
    V4l2PtzController,
    _clamp_speed,
    _direction_velocity,
    _parse_get_ctrl,
    _parse_onvif_host,
    _rtsp_uri_with_credentials,
    _v4l2_axis_sign,
    build_ptz_controller,
    capabilities_from_controls,
    handle_command,
    parse_v4l2_controls,
    resolve_capabilities,
    should_reopen_camera,
)


class FakeWebSocket:
    def __init__(self):
        self.messages = []

    def send(self, message):
        self.messages.append(json.loads(message))


class FakePtzController:
    def __init__(self):
        self.moves = []
        self.patrols = 0

    def move(self, direction):
        self.moves.append(direction)

    def patrol(self):
        self.patrols += 1

    def capabilities(self):
        return ["ptz", "patrol"]


class CameraClientPtzTests(unittest.TestCase):
    FULL_PTZ = (
        "        pan_absolute 0x009a0908 (int)    : min=-36000 max=36000 step=3600 default=0 value=0\n"
        "       tilt_absolute 0x009a0909 (int)    : min=-36000 max=36000 step=3600 default=0 value=0\n"
        "       zoom_absolute 0x009a090d (int)    : min=100 max=400 step=1 default=100 value=100\n"
    )
    ZOOM_ONLY = (
        "       zoom_absolute 0x009a090d (int)    : min=100 max=500 step=1 default=100 value=100\n"
        "power_line_frequency 0x00980918 (menu)   : min=0 max=2 default=1 value=1\n"
    )
    BCC950_RELATIVE = (
        "        pan_relative 0x009a0904 (int)    : min=-1 max=1 step=1 default=0 value=0\n"
        "       tilt_relative 0x009a0905 (int)    : min=-1 max=1 step=1 default=0 value=0\n"
    )

    def test_parse_onvif_host_uses_config_scheme_and_port(self):
        scheme, host, port = _parse_onvif_host("192.168.1.73", {"scheme": "http"})
        self.assertEqual((scheme, host, port), ("http", "192.168.1.73", None))

    def test_parse_onvif_host_preserves_url_port(self):
        scheme, host, port = _parse_onvif_host("https://192.168.1.73:443", {})
        self.assertEqual((scheme, host, port), ("https", "192.168.1.73", 443))

    def test_clamp_speed_normalizes_to_onvif_range(self):
        self.assertEqual(_clamp_speed(0.0), 0.05)
        self.assertEqual(_clamp_speed(-0.2), 0.2)
        self.assertEqual(_clamp_speed(2.0), 1.0)

    def test_rtsp_uri_with_credentials_adds_userinfo(self):
        uri = _rtsp_uri_with_credentials("rtsp://192.168.1.73:554/stream1", "user", "p@ss")
        self.assertEqual(uri, "rtsp://user:p%40ss@192.168.1.73:554/stream1")

    def test_rtsp_uri_with_credentials_preserves_existing_userinfo(self):
        uri = _rtsp_uri_with_credentials("rtsp://old:creds@192.168.1.73/stream1", "user", "pass")
        self.assertEqual(uri, "rtsp://old:creds@192.168.1.73/stream1")

    def test_direction_velocity_supports_pan_inversion(self):
        self.assertEqual(_direction_velocity("pan_left", 0.2, 0.3, False, False), (-0.2, 0.0))
        self.assertEqual(_direction_velocity("pan_left", 0.2, 0.3, True, False), (0.2, 0.0))

    def test_onvif_call_reconnects_and_retries_once(self):
        controller = OnvifPtzController.__new__(OnvifPtzController)
        reconnects = []
        attempts = {"count": 0}

        def reconnect():
            reconnects.append("reconnected")

        def flaky_call():
            attempts["count"] += 1
            if attempts["count"] == 1:
                raise RuntimeError("stale connection")
            return "ok"

        controller._connect = reconnect

        self.assertEqual(controller._call_with_reconnect("ContinuousMove", flaky_call), "ok")
        self.assertEqual(reconnects, ["reconnected"])
        self.assertEqual(attempts["count"], 2)

    def test_should_reopen_camera_after_threshold(self):
        self.assertFalse(should_reopen_camera(9, 10))
        self.assertTrue(should_reopen_camera(10, 10))
        self.assertFalse(should_reopen_camera(10, 0))

    def test_parse_v4l2_controls_keeps_only_ptz(self):
        controls = parse_v4l2_controls(self.ZOOM_ONLY)
        self.assertIn("zoom_absolute", controls)
        self.assertNotIn("power_line_frequency", controls)
        self.assertEqual(controls["zoom_absolute"]["max"], 500)

    def test_capabilities_from_controls(self):
        self.assertEqual(
            capabilities_from_controls(parse_v4l2_controls(self.FULL_PTZ)),
            ["ptz", "patrol", "zoom"],
        )
        self.assertEqual(
            capabilities_from_controls(parse_v4l2_controls(self.ZOOM_ONLY)),
            ["zoom"],
        )

    def test_v4l2_axis_sign(self):
        self.assertEqual(_v4l2_axis_sign("pan_left"), ("pan", -1))
        self.assertEqual(_v4l2_axis_sign("zoom_in"), ("zoom", 1))
        with self.assertRaises(ValueError):
            _v4l2_axis_sign("bogus")

    def test_parse_get_ctrl(self):
        self.assertEqual(_parse_get_ctrl("pan_absolute: -7200\n", "pan_absolute"), -7200)
        self.assertIsNone(_parse_get_ctrl("other: 3", "pan_absolute"))

    def _v4l2_controller(self, controls_text, get_value=0):
        calls = []

        def runner(args):
            calls.append(args)
            if any("--get-ctrl" in a for a in args):
                name = args[-1].split("=", 1)[1]
                return f"{name}: {get_value}\n"
            if any("--list-ctrls" in a for a in args):
                return controls_text
            return ""

        cfg = {"camera": {"device_index": 0}, "ptz": {}}
        return V4l2PtzController(cfg, runner=runner), calls

    def test_v4l2_absolute_move_clamps(self):
        controller, calls = self._v4l2_controller(self.FULL_PTZ, get_value=34000)
        controller.move("pan_right")
        set_call = calls[-1]
        self.assertIn("--set-ctrl=pan_absolute=36000", set_call)

    def test_v4l2_zoom_absolute_clamps(self):
        # zoom_absolute range is [100, 400]; from 380, +step_zoom(50) -> 430,
        # clamped to the max. Exercises the zoom axis the branch adds.
        controller, calls = self._v4l2_controller(self.FULL_PTZ, get_value=380)
        controller.move("zoom_in")
        self.assertIn("--set-ctrl=zoom_absolute=400", calls[-1])

    def test_v4l2_absolute_move_without_max_does_not_freeze(self):
        # A control parsed without max= must not clamp the target back to the
        # current value (which silently froze the axis); it advances by step.
        calls = []

        def runner(args):
            calls.append(args)
            if any("--get-ctrl" in a for a in args):
                return "zoom_absolute: 100\n"
            return ""

        cfg = {"camera": {"device_index": 0}, "ptz": {}}
        controller = V4l2PtzController(
            cfg, runner=runner, controls={"zoom_absolute": {"min": 100}}
        )
        controller.move("zoom_in")
        self.assertIn("--set-ctrl=zoom_absolute=150", calls[-1])

    def test_v4l2_relative_move_clamps_to_range(self):
        # BCC950 *_relative range is [-1, 1]; the default step_pan=3600 must be
        # clamped to the control's range, not written raw as -3600.
        controller, calls = self._v4l2_controller(self.BCC950_RELATIVE)
        controller.move("pan_left")
        self.assertIn("--set-ctrl=pan_relative=-1", calls[-1])

    def test_v4l2_capabilities(self):
        controller, _ = self._v4l2_controller(self.FULL_PTZ)
        self.assertEqual(controller.capabilities(), ["ptz", "patrol", "zoom"])

    def test_v4l2_unsupported_axis_raises(self):
        controller, _ = self._v4l2_controller(self.ZOOM_ONLY)
        with self.assertRaises(ValueError):
            controller.move("pan_left")

    def test_onvif_move_zoom_sends_zoom_velocity(self):
        controller = OnvifPtzController.__new__(OnvifPtzController)
        controller.zoom_speed = 0.4
        controller.invert_zoom = False
        controller.move_seconds = 0.0
        sent = {}

        def fake_continuous(pan, tilt, zoom):
            sent["v"] = (pan, tilt, zoom)

        controller._continuous_move = fake_continuous
        controller.stop = lambda **kwargs: sent.setdefault("stopped", kwargs)

        controller.move("zoom_in")
        self.assertEqual(sent["v"], (0.0, 0.0, 0.4))

    def test_onvif_capabilities_includes_zoom_when_supported(self):
        controller = OnvifPtzController.__new__(OnvifPtzController)
        controller.supports_zoom = True
        self.assertEqual(controller.capabilities(), ["ptz", "patrol", "zoom"])
        controller.supports_zoom = False
        self.assertEqual(controller.capabilities(), ["ptz", "patrol"])

    def test_resolve_capabilities_adds_ptz_when_controller_ready(self):
        caps = resolve_capabilities(["custom"], FakePtzController())
        self.assertEqual(caps, ["custom", "ptz", "patrol"])

    def test_resolve_capabilities_preserves_configured_without_controller(self):
        caps = resolve_capabilities(["ptz"], None)
        self.assertEqual(caps, ["ptz"])

    def test_ptz_command_without_controller_fails_ack(self):
        ws = FakeWebSocket()
        changed_view = handle_command(
            ws,
            {"action": "ptz", "params": {"direction": "pan_left"}},
            "cam1",
            None,
        )
        self.assertFalse(changed_view)
        self.assertFalse(ws.messages[0]["success"])
        self.assertEqual(ws.messages[0]["action"], "ptz")

    def test_ptz_command_calls_controller(self):
        ws = FakeWebSocket()
        ptz = FakePtzController()
        changed_view = handle_command(
            ws,
            {"action": "ptz", "params": {"direction": "pan_right"}},
            "cam1",
            ptz,
        )
        self.assertTrue(changed_view)
        self.assertTrue(ws.messages[0]["success"])
        self.assertEqual(ptz.moves, ["pan_right"])

    def test_patrol_command_calls_controller(self):
        ws = FakeWebSocket()
        ptz = FakePtzController()
        changed_view = handle_command(ws, {"action": "patrol", "params": {}}, "cam1", ptz)
        self.assertTrue(changed_view)
        self.assertTrue(ws.messages[0]["success"])
        self.assertEqual(ptz.patrols, 1)

    def test_zoom_command_calls_controller(self):
        ws = FakeWebSocket()
        ptz = FakePtzController()
        changed = handle_command(
            ws, {"action": "zoom", "params": {"direction": "zoom_in"}}, "cam1", ptz
        )
        self.assertTrue(changed)
        self.assertTrue(ws.messages[0]["success"])
        self.assertEqual(ptz.moves, ["zoom_in"])

    def test_zoom_command_without_controller_reports_zoom(self):
        # The merged ptz/zoom arm must label the no-controller error per action,
        # so a failed zoom reads "Zoom ..." rather than "PTZ ...".
        ws = FakeWebSocket()
        changed_view = handle_command(
            ws,
            {"action": "zoom", "params": {"direction": "zoom_in"}},
            "cam1",
            None,
        )
        self.assertFalse(changed_view)
        self.assertFalse(ws.messages[0]["success"])
        self.assertIn("Zoom", ws.messages[0]["message"])

    def test_command_with_null_params_does_not_crash(self):
        # An explicit "params": null from the server must coerce to {} instead
        # of raising AttributeError on params.get(...).
        ws = FakeWebSocket()
        ptz = FakePtzController()
        changed_view = handle_command(
            ws,
            {"action": "ptz", "params": None},
            "cam1",
            ptz,
        )
        self.assertTrue(changed_view)
        self.assertTrue(ws.messages[0]["success"])
        self.assertEqual(ptz.moves, [""])

    def test_build_ptz_controller_uses_v4l2_for_local(self):
        cfg = {"camera": {"source_type": "local", "device_index": 0}, "ptz": {}}

        def runner(args):
            return self.FULL_PTZ if any("--list-ctrls" in a for a in args) else ""

        import camera_client

        original = camera_client._v4l2_run
        camera_client._v4l2_run = runner
        try:
            controller = build_ptz_controller(cfg)
        finally:
            camera_client._v4l2_run = original
        self.assertIsInstance(controller, V4l2PtzController)
        self.assertEqual(controller.capabilities(), ["ptz", "patrol", "zoom"])


if __name__ == "__main__":
    unittest.main()
