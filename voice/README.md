# Agenta Voice Pipeline

"Hey Dalang" → Whisper STT → Agenta → Kokoro TTS → Speakers

## Setup

### 1. Install whisper.cpp

```bash
brew install whisper-cpp

# Download model
mkdir -p ~/.local/share/whisper
wget -O ~/.local/share/whisper/ggml-large-v3-turbo.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin
```

### 2. Start Kokoro TTS

```bash
cd voice/
docker compose up -d
```

### 3. Install Python dependencies

```bash
# Install portaudio (required by pyaudio)
brew install portaudio

# Create virtual environment with Python 3.13
cd voice/
python3.13 -m venv venv

# Install dependencies
venv/bin/pip install -r requirements.txt
```

### 4. (Optional) Train custom wake word

```bash
python -m openwakeword.train --phrase "hey dalang"
# Then update WAKE_WORD_MODEL in pipeline.py to the generated model path
```

### 5. Create voice agent in Agenta

```bash
agenta create \
  --name "dalang" \
  --model "gemma4:e4b" \
  --memory \
  --prompt "You are Dalang, a personal AI assistant running locally on Arif's Mac Mini.
You have a warm, direct personality. This is a voice conversation — respond in natural
spoken sentences only. No bullet points, no markdown, no lists. Keep responses concise
— 2 to 4 sentences unless asked for more. You remember past conversations and refer
back to them naturally. When greeted, respond warmly and briefly."
```

### 6. Run

```bash
cd voice/
venv/bin/python pipeline.py
```

## Config

Edit the CONFIG section at the top of `pipeline.py`:

| Variable | Default | Description |
|---|---|---|
| `AGENTA_AGENT_NAME` | `dalang` | Agent name in agenta |
| `KOKORO_VOICE` | `af_sarah` | TTS voice |
| `WAKE_WORD_MODEL` | `hey_jarvis` | Wake word (or custom model path) |
| `WAKE_WORD_THRESH` | `0.5` | Detection sensitivity (0.0–1.0) |
| `SILENCE_DURATION` | `1.5` | Seconds of silence to stop recording |

## Available Kokoro Voices

- `af_sarah` — American female, natural
- `af_bella` — American female, warm
- `am_adam` — American male
- `bf_emma` — British female
- `bm_george` — British male

## Latency Expectations (Mac Mini M-series)

| Step | Time |
|---|---|
| Wake word | instant |
| Recording | until you stop talking |
| Whisper STT | ~500ms |
| Agenta (local 7B) | ~2–3s |
| First audio (Kokoro) | ~300ms after response |
| **Total to first audio** | **~3–4s** |
