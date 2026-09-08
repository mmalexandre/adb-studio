#!/usr/bin/env python3
"""Recreate missing AceStep workflow files from ordered generation notes."""

from __future__ import annotations

import argparse
import copy
import json
import re
from pathlib import Path


AUDIO_EXTENSIONS = {".flac"}
MARKER = {"recreated_by_script": True}
KEYS = re.compile(
    r"^(?:[A-G](?:#|b)?)(?:\s+(?:major|minor))$", re.IGNORECASE
)
FIELD_PATTERNS = {
    "bpm": re.compile(r"^\s*(?:bpm|tempo)\s*(?:is|maybe|:)??\s*(\d+)\s*$", re.I),
    "seed": re.compile(r"^\s*seed\s*(?:is|:)??\s*(\d+)\s*$", re.I),
}
LORA_LINE = re.compile(r"^\s*lora\s*([^,\n]*?)(?:\s+(\d+(?:\.\d+)?))?\s*$", re.I)


def audio_files(root: Path) -> list[Path]:
    return sorted(
        (path for path in root.rglob("*") if path.is_file() and path.suffix.lower() in AUDIO_EXTENSIONS),
        key=lambda path: (path.stat().st_mtime_ns, str(path).lower()),
    )


def note_for(audio: Path, notes: Path) -> Path:
    return notes / f"{audio.name}.txt"


def parse_note(text: str, previous: dict) -> dict:
    state = copy.deepcopy(previous)
    lines = text.splitlines()
    prose: list[str] = []
    lyrics: list[str] = []
    in_lyrics = False
    for line in lines:
        stripped = line.strip()
        for field, pattern in FIELD_PATTERNS.items():
            match = pattern.match(line)
            if match:
                state[field] = match.group(1)
        if KEYS.match(stripped):
            state["key"] = stripped
        lora = LORA_LINE.match(line)
        if lora and (stripped.lower().startswith("lora") or stripped.lower().startswith("lora_")):
            name = lora.group(1).strip(" _:-")
            strength = lora.group(2)
            state.setdefault("loras", [])
            if name:
                state["loras"] = [{"filename": name, "strength": strength or ""}]
        if stripped.startswith("[") and "]" in stripped:
            in_lyrics = True
        if in_lyrics:
            lyrics.append(line)
        elif stripped and not FIELD_PATTERNS["bpm"].match(line) and not FIELD_PATTERNS["seed"].match(line) and not KEYS.match(stripped) and not lora:
            prose.append(stripped)
    if prose:
        state["prompt"] = " ".join(prose)
    if lyrics:
        state["lyrics"] = "\n".join(lyrics).strip()
    return state


def scalar(value):
    return value if isinstance(value, (str, int, float, bool)) else str(value)


def widget_indices(node: dict) -> dict[str, int]:
    indices = {}
    widget_values = node.get("widgets_values", [])
    index = 0
    for item in node.get("inputs", []):
        if "widget" not in item:
            continue
        name = str(item.get("name", "")).lower()
        indices[name] = index
        index += 1
        if name == "seed" and index < len(widget_values) and str(widget_values[index]).lower() in {"fixed", "randomize"}:
            index += 1
    return indices


def set_field(node: dict, field: str, value: str) -> bool:
    values = node.get("widgets_values")
    if not isinstance(values, list):
        return False
    indices = widget_indices(node)
    aliases = {
        "prompt": ("tags", "prompt", "text"),
        "lyrics": ("lyrics", "lyric"),
        "bpm": ("bpm",),
        "seed": ("seed",),
        "key": ("keyscale", "key", "tonality"),
    }
    for alias in aliases[field]:
        if alias in indices and indices[alias] < len(values):
            values[indices[alias]] = scalar(value)
            return True
    return False


def update_workflow(workflow: dict, state: dict) -> None:
    for node in workflow.get("nodes", []):
        node_type = str(node.get("type", "")).lower()
        if "textencodeacestep" in node_type:
            for field in ("prompt", "lyrics", "bpm", "seed", "key"):
                set_field(node, field, state.get(field, ""))
            values = node.get("widgets_values", [])
            if len(values) >= 9 and not widget_indices(node):
                values[0], values[1], values[2], values[4], values[8] = (
                    state.get("prompt", ""), state.get("lyrics", ""), state.get("seed", ""),
                    state.get("bpm", ""), state.get("key", ""),
                )
        if "loraloader" in node_type or "load lora" in node_type:
            loras = state.get("loras", [])
            if not loras:
                continue
            values = node.get("widgets_values", [])
            if values:
                values[0] = loras[0]["filename"]
            if len(values) > 1 and loras[0]["strength"]:
                values[1] = scalar(loras[0]["strength"])


def find_template(root: Path, explicit: Path | None) -> Path:
    if explicit:
        return explicit
    candidate_directories = [
        root / "datasets" / "output" / "workflows",
        root / "lora_training" / "datasets" / "output" / "workflows",
    ]
    candidates = sorted(
        candidate
        for directory in candidate_directories
        for candidate in directory.glob("*.json")
    )
    for candidate in candidates:
        try:
            workflow = json.loads(candidate.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        if any(
            "textencodeacestep" in str(node.get("type", "")).lower()
            and "audio" in str(node.get("type", "")).lower()
            for node in workflow.get("nodes", [])
        ):
            return candidate
    raise FileNotFoundError("No visual AceStep workflow template found; pass --template")


def workflow_path(audio: Path) -> Path:
    return audio.with_name(f"{audio.name}.workflow.json")


def marker_path(audio: Path) -> Path:
    return audio.with_name(f"{audio.name}.metadata.json")


def recreate(root: Path, notes: Path, template: Path, force: bool) -> int:
    state = {"bpm": "", "seed": "", "key": "", "prompt": "", "lyrics": "", "loras": []}
    created = 0
    template_value = json.loads(template.read_text())
    for audio in audio_files(root):
        output = workflow_path(audio)
        marker = marker_path(audio)
        note = note_for(audio, notes)
        if note.is_file():
            state = parse_note(note.read_text(), state)
        if output.exists() and not force:
            continue
        workflow = copy.deepcopy(template_value)
        update_workflow(workflow, state)
        output.write_text(json.dumps(workflow, indent=2) + "\n")
        marker.write_text(json.dumps(MARKER, indent=2) + "\n")
        created += 1
        print(output)
    return created


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path, help="audio root, e.g. .../acestep_15_turbo/downloads")
    parser.add_argument("--notes", type=Path)
    parser.add_argument("--template", type=Path)
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    notes = args.notes or args.root.parent / "lora_training" / "notes"
    template = find_template(args.root.parent, args.template)
    print(f"Created {recreate(args.root, notes, template, args.force)} workflow files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())