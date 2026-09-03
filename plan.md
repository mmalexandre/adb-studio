# Plan: Adb Studio - Slint + Rust Audio Metadata Application

## Project Decisions
- **Project Structure**: Single Cargo binary with modular code organization (easier for Rust beginner)
- **Audio Libraries**: Symphonia (better format support: MP3, WAV, Opus, FLAC) + hound (waveform reading)
- **Version Management**: Git-based (git describe) + fallback to Cargo.toml
- **Metadata Schema**: Hierarchical (.adbstudio/index.json + metadata/*.json per audio)
- **LoRA Parsing**: From "Load LoRa" nodes in ComfyUI JSON
- **ComfyUI Parsing**: Flexible (extract what's found, skip what's missing)
- **Waveform Cache**: Include app version in cache key for invalidation
- **MVP Approach**: Phased delivery to ensure stability at each step

## High-Level Architecture

### Backend (Rust)
- `src/main.rs` - Application entry point
- `src/ui/` - Slint UI definitions and bindings
- `src/app/` - Core application logic
  - `file_system.rs` - Directory traversal, file filtering, permissions
  - `audio/` - Audio playback and waveform generation
    - `playback.rs` - Rodio-based playback engine
    - `waveform.rs` - Waveform generation and caching
  - `metadata/` - Metadata management
    - `store.rs` - JSON persistence (.adbstudio/)
    - `comfyui.rs` - ComfyUI workflow parsing
    - `schema.rs` - Metadata structures (serde)

### Frontend (Slint)
- Main window structure
- Menu bar (File, Help)
- Left pane (icon toolbar + file explorer)
- Center pane (welcome, audio list with waveforms)
- Bottom pane (metadata viewer, initially collapsed)

### Data Flow
1. User opens folder → App scans directory structure
2. User selects folder in explorer → Load audio files from that folder
3. User selects ComfyUI JSON → Parse and extract metadata
4. Waveform generation on-demand with caching in .adbstudio/
5. All metadata stored in .adbstudio/index.json + individual files

---

## Phase 1: Project Setup + UI Shell (Foundational)

### Goals
- Slint project skeleton running
- Menu bar (File > Open Folder, Help > About)
- About dialog with version/build info
- Welcome screen placeholder in center
- Directory chooser integration

### Steps
1. Initialize Cargo project with Slint dependency
   - Set up Cargo.toml with dependencies: slint, tokio, serde, serde_json
   - Configure build.rs for git-based versioning (git describe)
2. Create Slint UI definition (main.slint)
   - Window layout: menu bar at top, placeholder for content
   - Menu definitions: File { Open Folder... }, Help { About }
3. Create About dialog (modal window with version, build, author info)
4. Create Welcome screen (center pane when no folder selected)
5. Implement Rust backend structure
   - `src/main.rs` with Tokio runtime and Slint event loop
   - `src/app/mod.rs` basic app state
   - Platform-specific folder picker (native-dialog or similar)
6. Wire menu actions to Rust backend
   - Open Folder → Show native file dialog → Update app state
   - About → Show modal
7. **Verification**:
   - Run `cargo build` successfully
   - Launch app, see menu bar
   - File > Open Folder opens a folder picker
   - Help > About shows dialog with version
   - Welcome screen displays when no folder is open

---

## Phase 2: File Explorer & Icon Toolbar (UI Navigation)

### Goals
- Left pane with vertical icon toolbar
- File explorer showing directory tree
- Left pane toggle on icon click
- File/folder icons (generic for now)

### Steps
1. Extend Slint UI: Add left pane with icon toolbar
   - Vertical button bar (single "file explorer" icon initially)
   - Collapsible/toggleable left panel
   - Directory tree view (Slint StandardTreeView or custom)
2. Implement backend file system module (`src/app/file_system.rs`)
   - Recursive directory traversal (async)
   - File type detection (audio, json, safetensors)
   - Sorting options (by name, by date, etc.)
   - Permission-aware listing (skip inaccessible dirs)
3. Bind file explorer to Rust backend
   - Load directory structure on "Open Folder"
   - Populate tree view dynamically
   - Detect double-click on folder → expand/navigate
4. Add file icons in Slint
   - Audio file icon (MP3, WAV, Opus, FLAC)
   - JSON file icon
   - Safetensors file icon
   - Folder icon
5. **Verification**:
   - Open a folder with mixed files
   - File explorer shows correct hierarchy
   - Icons render correctly
   - Icon toolbar toggles left pane visibility
   - No errors on deep/large folder structures

---

## Phase 3: Audio List & Waveform Generation (Audio Core)

### Goals
- Center pane shows audio files from selected folder
- Filter input (real-time partial match on filename)
- Waveform generation and display
- Waveform caching in .adbstudio/
- Sort by modified date (descending) by default

### Steps
1. Create metadata store structure (`src/app/metadata/schema.rs`)
   - AudioFileMetadata: { file_path, rating, comments[], modified_date, waveform_cache_key }
   - Implement serde for JSON serialization
   - Define .adbstudio/index.json schema
2. Implement audio waveform module (`src/app/audio/waveform.rs`)
   - Use Symphonia to decode MP3/WAV/Opus/FLAC
   - Generate waveform data (peaks per pixel or bucket)
   - Compute file hash (SHA256) for cache invalidation
   - Include app version in cache key (via build.rs)
   - Store waveforms in .adbstudio/waveforms/{hash}.json
3. Update file system module to filter only audio files from selected folder
   - Hook into metadata store to load existing waveforms
   - Queue missing waveforms for generation
4. Extend Slint UI: Center pane audio list
   - Filter input field at top
   - Scrollable list of audio files
   - Each item: waveform display (placeholder graphic for now) + filename
   - Implement filtering logic (Rust-side, update UI on keystroke)
5. Implement waveform rendering in Slint
   - Draw waveform as SVG or canvas (Slint supports both)
   - Simple two-channel visualization
   - Cache rendered images in .adbstudio/ if performance needed
6. Load and persist metadata
   - On folder open, load .adbstudio/index.json if exists
   - On audio list update, save index back to disk
7. **Verification**:
   - Open folder with audio files
   - Waveforms generate (check .adbstudio/waveforms/)
   - Filter input works in real-time
   - List sorted by date descending
   - Metadata persists across app restarts
   - App version in cache key (verify by inspecting .adbstudio/ file names)

---

## Phase 4: Audio Playback & Timeline (Playback Engine)

### Goals
- Play/pause button per audio file
- Waveform as interactive seekbar
- Timeline display (current time / total time)
- White cursor line on waveform
- Orange/gray coloring (played vs. unplayed)
- Seek by clicking on waveform
- Auto-play starts on cursor click without play button

### Steps
1. Implement audio playback module (`src/app/audio/playback.rs`)
   - Use Rodio for cross-platform playback
   - Manage playback state (playing, stopped, position)
   - Expose play(), pause(), seek(position), get_duration(), get_position()
   - Handle multiple file opens (stop current, start new)
2. Add audio state to app backend
   - Currently playing file path, position, duration
   - Playback source (Rodio decoder)
   - Update position periodically (via Tokio timer)
3. Extend Slint UI: Playback controls
   - Play/pause button per audio item (or one global)
   - Time display: "00:30 / 03:45" format (seconds + milliseconds)
   - Waveform click-to-seek logic
   - White cursor line (overlay on waveform)
4. Implement waveform cursor and color logic
   - Divide waveform into "played" (left of cursor) and "unplayed" (right)
   - Render played portion in orange, unplayed in light gray
   - Update cursor position in real-time during playback
5. Implement seek-by-click
   - On waveform click, seek to clicked time
   - If not playing, start playback at that position
6. **Verification**:
   - Click play/pause, audio plays and stops
   - Seek by clicking waveform
   - Time display updates in real-time
   - Cursor moves with playback
   - Waveform colors change correctly (orange/gray)
   - Multiple files: switching stops old, plays new

---

## Phase 5: Star Rating & Comments (Annotation Features)

### Goals
- 5-star rating widget (hover + click)
- Reset to zero stars (hover over 0 area or dedicated button)
- Right-click waveform → point comment popup
- Right-click-drag waveform → ranged comment selection and popup
- Display selected ranges in orange over the gray waveform with black endpoint locators
- Comment display below waveform (truncated, full on hover)
- Click a comment to restore its range markers
- Edit/delete comments and reposition their start/end range
- General Loop control with persistent enabled/disabled state
- Save ratings and ranged comments to .adbstudio/ metadata

### Steps
1. Extend metadata schema (`src/app/metadata/schema.rs`)
   - Add fields: `rating: 0..=5`, `comments: [{ start_seconds, end_seconds, text }]`
   - A point comment uses equal start and end values
2. Extend Slint UI: Star rating widget
   - 5 star buttons (or custom star glyph)
   - Hover state (preview rating)
   - Click to set rating
   - Visual indicator for current rating
   - Right-click or icon to reset to 0
3. Add comment range interaction and popup UI
   - Right click creates a zero-length range
   - Right-click drag creates a start/end range and highlights it orange
   - Show black vertical endpoint lines with downward triangles
   - Modal dialog with text input
   - Buttons: Save, Cancel, Delete (if existing)
   - Show current comment text if editing
4. Implement comment rendering
   - Display comments as small labels below waveform
   - Truncate with ellipsis ("This is a long comment...")
   - Tooltip/hover shows full text
5. Implement comment range editing and selection
   - Edit start and end positions from the selected comment
   - Clicking a comment restores both endpoint markers
   - Dragging a comment updates its range and keeps start ≤ end
6. Add general playback Loop control
   - Toggle Loop on/off beside the play controls
   - Persist the preference in application settings
   - When enabled, repeat the selected comment range; without a selected range, repeat the active track
7. Persist rating and comments
   - On app exit, save to .adbstudio/ (via existing metadata store)
8. **Verification**:
   - Click and hover star rating
   - Right-click waveform → Add point comment → Save
   - Right-click-drag waveform → verify orange range and endpoint markers
   - Comment appears below waveform and clicking it restores the range
   - Edit or drag a comment, verify both times changed in metadata
   - Edit existing comment
   - Delete comment
   - Toggle Loop and verify selected-range and full-track behavior
   - Restart app, metadata persists

---

## Phase 6: ComfyUI Metadata & Bottom Pane (Metadata Viewer)

### Goals
- Foldable bottom pane (closed by default, toggle with button)
- Dropdown to select ComfyUI JSON from workspace folder
- Parse and extract:
  - BPM (from TextEncodeAceStepAudio1.5 node)
  - Key (e.g., "Bb minor")
  - Prompt (from Acestep prompt field)
  - Lyrics
  - LoRA files and strengths (from Load LoRa nodes)
- Display in 4 columns: BPM/Key | Prompt | Lyrics | LoRAs
- Associate JSON with currently active audio file
- Flexible parsing (extract what exists, skip what's missing)

### Steps
1. Create ComfyUI parser module (`src/app/metadata/comfyui.rs`)
   - Define workflow schema (serde structures for Nodes, Links)
   - Parse "TextEncodeAceStepAudio1.5" node → extract BPM, key, prompt
   - Parse "Load LoRa" nodes → extract filename, strength
   - Search for lyrics field (flexible, may be in a text node)
   - Return structured data: `ComfyUIWorkflow { bpm, key, prompt, lyrics, loras }`
2. Add workflow state to app backend
   - Currently selected workflow JSON path
   - Parsed workflow data (cached)
   - Association: audio file → workflow JSON
3. Extend metadata schema
   - Add `workflow_json_path: Option<String>` to AudioFileMetadata
4. Extend Slint UI: Bottom pane
   - Hidden by default (collapsed)
   - Toggle button (e.g., chevron up/down)
   - Close button (X) to collapse
   - Dropdown to select JSON file from workspace
   - Layout: 4 columns (BPM/Key | Prompt | Lyrics | LoRAs)
5. Implement column rendering
   - Column 1: BPM label, Key label (small frames)
   - Column 2: Prompt text (full height, wrap text)
   - Column 3: Lyrics text (full height, wrap text)
   - Column 4: LoRA list (file names + strength as "x0.8" format)
6. Implement workflow JSON selection
   - Scan workspace folder for .json files
   - Populate dropdown on folder open
   - On selection, parse and display in pane
7. Persist workflow association
   - Save selected workflow path in .adbstudio/index.json
   - Load on app start
8. **Verification**:
   - Bottom pane hidden initially
   - Toggle button shows/hides pane
   - Dropdown lists JSON files in workspace
   - Select JSON, pane updates with parsed data
   - All 4 columns display correctly
   - Restart app, workflow association persists
   - Gracefully handle malformed JSON (no crash, show error)

---

## Phase 7: Polish & Edge Cases (Refinement)

### Goals
- Waveform caching optimization
- UI responsiveness (don't block on audio processing)
- Error handling and user feedback
- Cross-platform testing (Windows, macOS, Linux)
- Performance on large libraries (100+ files)

### Steps
1. Implement background waveform generation
   - Move waveform generation to background task (Tokio task pool)
   - UI shows placeholder while generating
   - Cache update without blocking
2. Add error UI
   - Show toast/notification for file access errors
   - Handle corrupted audio files gracefully
3. Optimize waveform rendering
   - Lazy render visible items only (scrolling performance)
   - Consider image caching for rendered waveforms
4. Add loading indicators
   - Spinner while scanning folder
   - Progress bar while generating waveforms
5. Cross-platform testing
   - Test on Linux, Windows, macOS
   - Verify file paths (/, \, spaces, unicode)
   - Test menu bar platform-specific behavior
6. Performance profiling
   - Test with 100+ audio files in folder
   - Measure and optimize waveform cache hit rate
7. **Verification**:
   - Large folder (100+ files) opens without freezing
   - Waveforms generate in background
   - All errors show user-friendly messages
   - App builds and runs on Linux/Windows/macOS

---

## Relevant Files to Create

- `Cargo.toml` — Dependencies (slint, symphonia, rodio, serde, tokio, sha2, git-version)
- `build.rs` — Git version extraction
- `src/main.rs` — App entry, Tokio runtime, Slint event loop
- `src/app/mod.rs` — Core app state
- `src/app/file_system.rs` — Directory traversal, file filtering
- `src/app/audio/mod.rs` — Audio module root
- `src/app/audio/playback.rs` — Playback engine (Rodio)
- `src/app/audio/waveform.rs` — Waveform generation and caching
- `src/app/metadata/mod.rs` — Metadata module root
- `src/app/metadata/store.rs` — JSON persistence
- `src/app/metadata/schema.rs` — Data structures (serde)
- `src/app/metadata/comfyui.rs` — ComfyUI workflow parser
- `src/ui/main.slint` — Main UI definition

---

## Architecture Decisions

1. **Single Binary**: Simpler for Rust beginners, easier to distribute
2. **Async I/O**: Tokio for responsiveness during file scanning, waveform generation
3. **Hierarchical Metadata**: More scalable than flat files; index.json acts as manifest
4. **Version in Cache Key**: Ensures clean rebuild when app updates change waveform format
5. **Flexible ComfyUI Parsing**: Resilient to workflow variations in the wild
6. **Phased Delivery**: Each phase is independently testable and shippable

---

## Known Constraints & Future Considerations

1. **Waveform Precision**: Phase 3 uses bucketing; sub-second accuracy can be added later
2. **Performance**: Large libraries (1000+) may need pagination or virtual scrolling in Phase 7
3. **Web Version**: Desktop-first; backend is modular enough to decouple from Slint UI later
4. **Platform-Specific**: File picker and menu bar may need platform-specific tweaks (Phase 1)
5. **Comment Visualization**: Overlapping comments at same time need z-order management (Phase 5)
6. **LoRA Strength Parsing**: ComfyUI format varies; Phase 6 includes lenient parsing fallback
