#!/usr/bin/env python3
"""Vendor bus sniffer (read-only).

Listens on the AiStone MQTT broker (127.0.0.1:13688) and records every message so
the payloads the official control center sends can be compared with what
open-revo sends.

Usage:
    python tools/vendor-bus-sniff.py --seconds 120 --out vendor-bus.log

Then open the official control center, move the fan curve / backlight slider,
and share the log. Nothing is published: this script only subscribes.
"""
import argparse
import json
import time

import paho.mqtt.client as mqtt

TOPICS = [
    "Fan/Control", "Fan/Status", "Fan/Table",
    "Keyboard/Ctrl", "Keyboard/Status",
    "Setting/Control", "Setting/Status",
    "System/Control", "System/FanInfo",
    "LCHWOC/Control", "LCHWOC/Status",
    "Customize/Control", "Customize/Info",
]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=int, default=120)
    parser.add_argument("--out", default="vendor-bus.log")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=13688)
    parser.add_argument("--client-id", default="UWPClient_9")
    args = parser.parse_args()

    client = mqtt.Client(client_id=args.client_id, protocol=mqtt.MQTTv311)
    client.username_pw_set(
        f"UWPClient_User_{args.client_id.split('_')[-1]}",
        f"UWPClient_Pwd888881772688_{args.client_id.split('_')[-1]}",
    )

    def on_connect(c, userdata, flags, rc, properties=None):
        print(f"connected rc={rc}", flush=True)
        c.subscribe([(topic, 2) for topic in TOPICS])

    def on_message(c, userdata, msg):
        line = json.dumps(
            {"t": round(time.time(), 3), "topic": msg.topic,
             "payload": msg.payload.decode("utf-8", "replace")},
            ensure_ascii=False,
        )
        print(line, flush=True)
        with open(args.out, "a", encoding="utf-8") as handle:
            handle.write(line + "\n")

    client.on_connect = on_connect
    client.on_message = on_message
    client.connect(args.host, args.port, 10)
    client.loop_start()
    time.sleep(args.seconds)
    client.loop_stop()
    print(f"saved {args.out}")


if __name__ == "__main__":
    main()
