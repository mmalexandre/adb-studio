# Current Plan: ComfyUI File Synchronization

## Goal

Add ComfyUI cloud-file synchronization to the Rust application. Users configure a ComfyUI endpoint, a download directory within the open workspace, and a polling frequency. The application validates the connection, persists the configuration, and downloads newly available files in the background.

## User Interface

1. Add a `cloud-sync` icon button to the playback bar, after the volume bar.
2. Show a ComfyUI synchronization modal when the button is clicked.
3. The modal collects:
   - ComfyUI instance URL, such as `https://nhqa3yjdminbod-8188.proxy.runpod.net/`
   - Download directory, selected within the current workspace
   - Synchronization frequency, defaulting to 4000 ms
4. On save, test the connection before accepting the configuration.
5. Keep the modal open and display an error when connection validation fails.
6. Close the modal when validation succeeds and persist the configuration in the workspace `.adbstudio` settings.
7. Render the cloud-sync icon in orange text color, without a background or border, when synchronization is configured and active.
8. Provide a hover status for the cloud-sync icon.

## Synchronization Service

1. Add a background process that polls the configured ComfyUI endpoint at the saved frequency.
2. Use the existing `comfyui_audio_list/list_audio_files.py` prototype as the behavioral reference for the remote API calls and file listing.
3. Retrieve the files that need downloading from ComfyUI.
4. Download eligible files to the configured directory inside the active workspace.
5. Maintain status suitable for the icon hover feedback, including whether the endpoint is configured, currently connecting, synchronized, or failed.

## Persistence And Validation

1. Extend workspace `.adbstudio` settings with the URL, destination directory, frequency, and enabled/status-relevant sync configuration.
2. Reject or surface invalid download directories outside the current workspace.
3. Preserve the configured state when reopening the workspace.
4. Ensure failed connection tests do not incorrectly mark synchronization as active.

## Verification

1. Confirm the playback-bar icon appears after the volume control.
2. Confirm the modal saves a valid endpoint, workspace-local download directory, and custom frequency.
3. Confirm invalid/unreachable endpoints show an error and leave the modal open.
4. Confirm valid configuration persists after reopening the workspace and lights the icon orange.
5. Confirm the background poll lists and downloads remote files using the prototype-compatible API behavior.
6. Confirm hover feedback reports the current synchronization state.
