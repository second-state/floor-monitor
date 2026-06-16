import json
import unittest

from camera_client import (
    OnvifPtzController,
    _clamp_speed,
    _direction_velocity,
    _parse_get_ctrl,
    _parse_onvif_host,
    _rtsp_uri_with_credentials,
    _v4l2_axis_sign,
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


if __name__ == "__main__":
    unittest.main()
