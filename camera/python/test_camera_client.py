import json
import unittest

from camera_client import (
    OnvifPtzController,
    _clamp_speed,
    _direction_velocity,
    _parse_onvif_host,
    _rtsp_uri_with_credentials,
    handle_command,
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
