#!/usr/bin/env python3
import argparse
import json
import sys
from urllib.parse import urlencode
from urllib.request import Request, urlopen


DEFAULT_URL = "https://nhqa3yjdminbod-8188.proxy.runpod.net"


def main():
    parser = argparse.ArgumentParser(description="List ComfyUI output/audio files.")
    parser.add_argument("--url", default=DEFAULT_URL, help="ComfyUI base URL")
    parser.add_argument("--directory", default="output/audio")
    args = parser.parse_args()

    endpoint = args.url.rstrip("/") + "/adb-music-player/audio-files?" + urlencode(
        {"directory": args.directory}
    )
    request = Request(
        endpoint,
        headers={
            "Accept": "application/json",
            "User-Agent": "comfyui-audio-list/1.0",
        },
    )

    with urlopen(request, timeout=20) as response:
        files = json.load(response)

    if not isinstance(files, list):
        raise RuntimeError("ComfyUI returned an unexpected response")

    for file_info in files:
        if isinstance(file_info, dict) and isinstance(file_info.get("name"), str):
            print(file_info["name"])

    print(f"\n{len(files)} file(s)", file=sys.stderr)


if __name__ == "__main__":
    main()