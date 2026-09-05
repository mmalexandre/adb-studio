# ComfyUI audio file list

This is a small, dependency-free Python client for the `ADB Music Player` custom node installed at `/var/www/comfyui_play` on the ComfyUI instance.

The node exposes:

```text
GET /adb-music-player/audio-files?directory=output/audio
```

## Setup and run

```sh
cd /var/www/adbstudio/comfyui_audio_list
python3 -m venv .venv
.venv/bin/python list_audio_files.py
```

The script uses the supplied server URL by default. Use another URL or directory when needed:

```sh
.venv/bin/python list_audio_files.py --url http://127.0.0.1:8188 --directory output/audio
```

No API key is required by the installed module. It exposes this read-only route without authentication.