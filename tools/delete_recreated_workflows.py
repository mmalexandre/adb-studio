#!/usr/bin/env python3
"""Delete only workflow and marker files created by recreate_workflows.py."""

import argparse
import json
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path)
    args = parser.parse_args()
    deleted = 0
    for marker in args.root.rglob("*.metadata.json"):
        try:
            data = json.loads(marker.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        if data != {"recreated_by_script": True}:
            continue
        workflow = marker.with_name(marker.name.removesuffix(".metadata.json") + ".workflow.json")
        marker.unlink()
        if workflow.is_file():
            workflow.unlink()
        deleted += 1
        print(workflow)
    print(f"Deleted {deleted} recreated workflow files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())