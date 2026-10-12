# OpenAssist

A featherweight voice assistant for Windows that lives in a black-glass notch at
the top of your screen. Talk to it or type to it, and it runs your PC: music,
volume, apps, folders, files, windows, settings, and clicking around inside apps.

It's free to run. Bring any one free AI tier (Gemini, Groq, or one-click
OpenRouter), and the most common commands work offline with no AI at all.

## Footprint (measured on this machine)

| | |
|---|---|
| Binary | **1.3 MB**, single exe, only Windows system DLLs, no installer |
| Idle CPU (asleep) | **0.0 ms over 10 s** |
| Idle memory (asleep) | **0.1 MB working set** (trimmed), ~15 MB committed |
| Disk while idle | **0 writes**. Config is written only when you change it. |

Why it's cheap:

- **UI:** pure Win32 + Direct2D (software) rendering on a layered window. No web
  view, no GPU device, no runtime.
- **Rendering:** a frame is drawn only when something changes. The pixel buffer
  is freed once the island is still, and DWM keeps showing the last frame.
- **Idle:** the UI thread sleeps in `GetMessage`. No timers or polling while
  idle, and the caret doesn't blink (a blink would repaint the panel twice a
  second).
- **Microphone:** opened only while you talk.
- **AI:** runs in the cloud, so there are no local models to load.
- **Efficiency mode:** EcoQoS is on while asleep, and the working set is
  trimmed after use.

## Using it

| | |
|---|---|
| **Talk** | Hold **Alt+Space** and speak, or tap it to talk hands-free (it stops when you pause). If another app owns Alt+Space, it falls back to **Ctrl+Alt+Space**, then **Ctrl+Shift+Space**. The menu footer shows the one in use. |
| **Type** | **Alt+Shift+Space** opens the console. ↑/↓ recall history, Esc closes. |
| **Quick menu** | Click the notch, or scroll down on it: now playing with prev/play/next, volume, Type, Talk, Settings. |
| **Tray** | Right-click for the menu, Settings, Quit. |

Things to try: "next song", "volume 30", "play Blinding Lights", "open
downloads", "move all PDFs from downloads to documents/pdfs", "snap Chrome
left", "click Export", "dark mode", "remind me in 10 minutes to stretch".

## Free AI providers

Add any one in Settings:

| Provider | How | Good for |
|---|---|---|
| **OpenRouter** | **Connect**: one-click sign-in (OAuth PKCE), no copy-paste. Picks a free model that supports tool calling. | Text commands. 50 free requests/day, or 1,000/day after a one-time $10 top-up. |
| **Gemini** | **Get free key**: copy it and the app picks it up from your clipboard. | Most generous free tier. Voice and text. |
| **Groq** | Same flow. | Fastest. Free Whisper speech-to-text. |

How requests are handled:

- **Fallback:** requests try providers in order and move to the next on rate
  limits, outages, or a bad key.
- **Offline fast path:** media, volume, open app/folder, lock, time, dark/light
  mode and screenshot are handled locally. They're instant and don't spend
  your quota.
- **Step cap:** a request takes at most 6 tool steps (`max_steps` in config) so
  one command can't burn your daily quota.
- **Keys:** stored DPAPI-encrypted in `%APPDATA%\OpenAssist\config.json` and
  sent only to that provider.

## Hard limits it's designed around

- **Admin windows, UAC prompts, lock screen:** Windows blocks other apps from
  controlling them. OpenAssist detects elevated targets and says so instead of
  failing silently, and never asks for admin rights itself.
- **Games and fullscreen:** the island hides during fullscreen apps,
  presentations and games, and never injects into them.
- **Spotify:** playback controls work through Windows' media session, with no
  login, for any player. Starting a specific song needs Spotify's developer API
  plus Premium, so "play X" opens Spotify's search. Say "click the first
  result" to finish.
- **Apps with poor accessibility:** clicking by name uses UI Automation. Apps
  that expose nothing (some games and custom UIs) get a clear "can't see
  controls" message, not blind clicks.
- **No terminal commands:** the assistant can't run shell commands.
- **Deletes:** files go to the Recycle Bin, only after an on-screen
  **Allow/Deny**. Moves and copies never overwrite. System folders and your
  profile root are off-limits.
- **Voice needs Gemini or Groq:** OpenRouter's free models can't hear.

## Build

```sh
cargo build --release      # -> target/release/openassist.exe
```

`rust-toolchain.toml` selects the GNU toolchain, which ships its own linker, so
the Visual Studio C++ workload isn't needed. If you have MSVC, `cargo
+stable-msvc build --release` works too.

Developer commands (no AI key needed):

```sh
openassist --tool list_files "{\"folder\":\"downloads\",\"pattern\":\"*.pdf\"}"
openassist --intent next song        # runs the offline parser: this executes!
openassist --free-models             # which free OpenRouter model would be picked
openassist --tts-test "hello"        # synthesises without playing
```

Set `OPENASSIST_LOG=1` to log to `%APPDATA%\OpenAssist\log.txt`. Logging is off
by default, so the app never writes on its own.

## Layout

```
src/
  main.rs       entry, single instance, EcoQoS, dev CLI
  island.rs     the notch: views, spring animation, input, hotkeys
  gfx.rs        Direct2D/DirectWrite on a layered window, notch geometry
  textbox.rs    single-line text field
  tray.rs       notification icon (painted at runtime)
  audio.rs      WASAPI capture, 16 kHz mono, simple VAD
  llm.rs        Gemini / Groq / OpenRouter client with fallback, STT
  oauth.rs      OpenRouter one-click sign-in
  agent.rs      fast path → model + tools loop
  intents.rs    offline command parser
  tts.rs        Windows speech synthesis
  tools/        media, volume, apps, files, windows, UI Automation, input, system
```

## Not built yet

- Screenshot plus vision fallback for apps without accessibility info. Costs 2–5
  requests per action, so it would be opt-in.
- "Circle to ask" (draw on screen, ask about it) via Gemini vision.
- Optional offline speech-to-text (sherpa-onnx + Moonshine), loaded only while
  listening.
- Opt-in wake word (always-on listening has a constant CPU cost).
- Configurable hotkeys in the UI, multi-monitor placement, persistent reminders.
