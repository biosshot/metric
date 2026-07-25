import json
import os
import sys
import threading

import sentry_sdk


class MetricPythonSdkCompatibilityError(RuntimeError):
    pass


def deadline() -> None:
    print(
        "real Python SDK sender exceeded its 15 second process deadline",
        file=sys.stderr,
    )
    os._exit(124)


if len(sys.argv) != 2:
    print("usage: send_error.py <dsn>", file=sys.stderr)
    raise SystemExit(2)

watchdog = threading.Timer(15, deadline)
watchdog.daemon = True
watchdog.start()
try:
    sentry_sdk.init(
        dsn=sys.argv[1],
        environment="sdk-compatibility",
        release="metric-python-sdk-test@1.0.0",
        traces_sample_rate=0,
        send_default_pii=False,
        auto_session_tracking=False,
        default_integrations=False,
        shutdown_timeout=5,
    )
    event_id = sentry_sdk.capture_exception(
        MetricPythonSdkCompatibilityError(
            "Metric real Python SDK compatibility event"
        )
    )
    sentry_sdk.get_client().close(timeout=5)
    print(json.dumps({"event_id": event_id, "flushed": True}))
finally:
    watchdog.cancel()
