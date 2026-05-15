#!/usr/bin/env python3
"""
Voice Pipeline for Agenta
--------------------------
Hey Dalang → Whisper STT → Agenta → Kokoro TTS → Speakers

Requirements:
  pip install openwakeword pyaudio sounddevice numpy requests scipy

External:
  - whisper.cpp binary (see README)
  - Kokoro TTS running on Docker (localhost:8880)
  - Agenta daemon running (localhost:8789)
"""

import io
import os
import sys
import wave
import time
import queue
import struct
import threading
import tempfile
import subprocess

import numpy as np
import pyaudio
import requests
import sounddevice as sd
from scipy.io import wavfile
from openwakeword.model import Model

# ─────────────────────────────────────────────
# CONFIG — edit these to match your setup
# ─────────────────────────────────────────────

AGENTA_URL        = "http://localhost:8789/api"
AGENTA_API_TOKEN  = os.getenv("AGENTA_API_TOKEN", "")
AGENTA_AGENT_NAME = "dalang"                        # your agent name in agenta

KOKORO_URL        = "http://localhost:8880/v1/audio/speech"
KOKORO_VOICE      = "af_sarah"                      # change to preferred voice
KOKORO_MODEL      = "kokoro"

WHISPER_BIN       = "/opt/homebrew/bin/whisper-cli"                 # whisper.cpp binary
WHISPER_MODEL     = os.path.expanduser("~/.local/share/whisper/ggml-large-v3-turbo.bin")

WAKE_WORD_MODEL   = "hey_jarvis"                    # or path to custom "hey_dalang" model
WAKE_WORD_THRESH  = 0.5

SAMPLE_RATE       = 16000
CHUNK_SIZE        = 1280                            # ~80ms at 16kHz
SILENCE_THRESH    = 500                             # RMS below this = silence
SILENCE_DURATION  = 1.5                             # seconds of silence to stop recording
MAX_RECORD_SECS   = 30                              # hard cap on recording length

# ─────────────────────────────────────────────
# STATE
# ─────────────────────────────────────────────

audio_queue   = queue.Queue()
is_speaking   = False   # true when TTS is playing — suppress wake word


# ─────────────────────────────────────────────
# AUDIO UTILITIES
# ─────────────────────────────────────────────

def rms(data: bytes) -> float:
    """Calculate RMS amplitude of raw PCM audio chunk."""
    count = len(data) // 2
    shorts = struct.unpack(f"{count}h", data)
    return (sum(s ** 2 for s in shorts) / count) ** 0.5


def record_until_silence() -> bytes:
    """Record audio until silence detected or max duration reached."""
    pa = pyaudio.PyAudio()
    stream = pa.open(
        rate=SAMPLE_RATE,
        channels=1,
        format=pyaudio.paInt16,
        input=True,
        frames_per_buffer=CHUNK_SIZE,
    )

    print("  🎙  Listening...")
    frames = []
    silent_chunks = 0
    silence_limit = int(SILENCE_DURATION * SAMPLE_RATE / CHUNK_SIZE)
    max_chunks = int(MAX_RECORD_SECS * SAMPLE_RATE / CHUNK_SIZE)

    for _ in range(max_chunks):
        chunk = stream.read(CHUNK_SIZE, exception_on_overflow=False)
        frames.append(chunk)
        if rms(chunk) < SILENCE_THRESH:
            silent_chunks += 1
            if silent_chunks >= silence_limit:
                break
        else:
            silent_chunks = 0

    stream.stop_stream()
    stream.close()
    pa.terminate()

    return b"".join(frames)


def save_wav(pcm: bytes) -> str:
    """Save raw PCM to a temp WAV file, return path."""
    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    with wave.open(tmp.name, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(SAMPLE_RATE)
        wf.writeframes(pcm)
    return tmp.name


# ─────────────────────────────────────────────
# STT — WHISPER.CPP
# ─────────────────────────────────────────────

def transcribe(wav_path: str) -> str:
    """Run whisper.cpp on a WAV file, return transcribed text."""
    try:
        result = subprocess.run(
            [WHISPER_BIN, "-m", WHISPER_MODEL, "-f", wav_path,
             "--no-timestamps", "-l", "en"],
            capture_output=True, text=True, timeout=30
        )
        text = result.stdout.strip()
        # whisper.cpp sometimes adds [BLANK_AUDIO] or similar
        if not text or "[" in text:
            return ""
        return text
    except subprocess.TimeoutExpired:
        print("  ⚠️  Whisper timed out")
        return ""
    except FileNotFoundError:
        print(f"  ❌  whisper.cpp binary not found at: {WHISPER_BIN}")
        sys.exit(1)
    finally:
        os.unlink(wav_path)


# ─────────────────────────────────────────────
# BRAIN — AGENTA
# ─────────────────────────────────────────────

def ask_agent(text: str) -> str:
    """Send text to agenta agent, return response."""
    headers = {}
    if AGENTA_API_TOKEN:
        headers["Authorization"] = f"Bearer {AGENTA_API_TOKEN}"

    try:
        # Trigger agent run
        run_resp = requests.post(
            f"{AGENTA_URL}/agents/{AGENTA_AGENT_NAME}/run",
            json={"input": text},
            headers=headers,
            timeout=60,
        )
        run_resp.raise_for_status()
        execution_id = run_resp.json().get("message") or run_resp.json().get("execution_id")

        # Poll for completion
        for _ in range(60):
            time.sleep(1)
            status_resp = requests.get(
                f"{AGENTA_URL}/executions/{execution_id}",
                headers=headers,
                timeout=10,
            )
            status_resp.raise_for_status()
            data = status_resp.json()
            if data.get("status") in ("completed", "failed", "cancelled"):
                return data.get("output", "").strip() or "I couldn't generate a response."

        return "Sorry, I took too long to respond."

    except requests.RequestException as e:
        print(f"  ❌  Agenta error: {e}")
        return "I'm having trouble reaching my brain right now."


# ─────────────────────────────────────────────
# TTS — KOKORO
# ─────────────────────────────────────────────

def split_sentences(text: str) -> list[str]:
    """Split text into sentences for streaming TTS."""
    import re
    sentences = re.split(r'(?<=[.!?])\s+', text.strip())
    return [s.strip() for s in sentences if s.strip()]


def speak(text: str):
    """Convert text to speech via Kokoro and play it."""
    global is_speaking
    is_speaking = True

    sentences = split_sentences(text)
    print(f"  🔊  Speaking: {text[:80]}{'...' if len(text) > 80 else ''}")

    for sentence in sentences:
        try:
            resp = requests.post(
                KOKORO_URL,
                json={
                    "model": KOKORO_MODEL,
                    "input": sentence,
                    "voice": KOKORO_VOICE,
                    "response_format": "wav",
                },
                stream=True,
                timeout=15,
            )
            resp.raise_for_status()

            # Stream audio chunks into buffer
            audio_data = b"".join(resp.iter_content(chunk_size=4096))

            # Decode WAV and play via sounddevice
            with io.BytesIO(audio_data) as buf:
                sample_rate, data = wavfile.read(buf)
                sd.play(data, samplerate=sample_rate)
                sd.wait()

        except requests.RequestException as e:
            print(f"  ⚠️  Kokoro error: {e}")
        except Exception as e:
            print(f"  ⚠️  Audio playback error: {e}")

    is_speaking = False


# ─────────────────────────────────────────────
# WAKE WORD DETECTION
# ─────────────────────────────────────────────

def load_wake_word_model() -> Model:
    """Load OpenWakeWord model."""
    try:
        model = Model(
            wakeword_models=[WAKE_WORD_MODEL],
            inference_framework="onnx",
        )
        return model
    except Exception as e:
        print(f"  ❌  Failed to load wake word model: {e}")
        print(f"      Try: python -m openwakeword.train --phrase 'hey dalang'")
        sys.exit(1)


def run_wake_word_listener(model: Model):
    """Main loop — listen for wake word, then run full pipeline."""
    pa = pyaudio.PyAudio()
    stream = pa.open(
        rate=SAMPLE_RATE,
        channels=1,
        format=pyaudio.paInt16,
        input=True,
        frames_per_buffer=CHUNK_SIZE,
    )

    print("\n✅  Voice pipeline ready.")
    print(f'👂  Listening for wake word: "{WAKE_WORD_MODEL}"')
    print("    (Ctrl+C to quit)\n")

    try:
        while True:
            chunk = stream.read(CHUNK_SIZE, exception_on_overflow=False)

            # Don't trigger while agent is speaking
            if is_speaking:
                continue

            # Run wake word detection
            audio_array = np.frombuffer(chunk, dtype=np.int16)
            predictions = model.predict(audio_array)

            if predictions.get(WAKE_WORD_MODEL, 0) >= WAKE_WORD_THRESH:
                print("\n🎯  Wake word detected!")
                stream.stop_stream()

                # Full pipeline in a thread so audio stream stays alive
                threading.Thread(
                    target=pipeline_run,
                    args=(stream, pa),
                    daemon=True,
                ).start()

                # Brief pause to avoid re-triggering
                time.sleep(0.5)
                stream.start_stream()

    except KeyboardInterrupt:
        print("\n\n👋  Shutting down voice pipeline.")
    finally:
        stream.stop_stream()
        stream.close()
        pa.terminate()


def pipeline_run(stream, pa):
    """Full pipeline: record → STT → agent → TTS."""
    try:
        # 1. Record until silence
        pcm = record_until_silence()

        # 2. Save to WAV
        wav_path = save_wav(pcm)

        # 3. Transcribe
        print("  🔤  Transcribing...")
        text = transcribe(wav_path)

        if not text:
            print("  ⚠️  Nothing transcribed — try again.")
            return

        print(f"  📝  You said: {text}")

        # 4. Ask agent
        print("  🧠  Thinking...")
        response = ask_agent(text)
        print(f"  💬  Agent: {response}")

        # 5. Speak
        speak(response)

    except Exception as e:
        print(f"  ❌  Pipeline error: {e}")


# ─────────────────────────────────────────────
# ENTRY POINT
# ─────────────────────────────────────────────

def main():
    print("─" * 50)
    print("  Agenta Voice Pipeline")
    print("─" * 50)
    print(f"  Agent    : {AGENTA_AGENT_NAME}")
    print(f"  Wake word: {WAKE_WORD_MODEL}")
    print(f"  Kokoro   : {KOKORO_URL}")
    print(f"  Whisper  : {WHISPER_BIN}")
    print("─" * 50)

    # Check Kokoro is reachable
    try:
        requests.get("http://localhost:8880/health", timeout=3)
        print("  ✅  Kokoro TTS: online")
    except requests.RequestException:
        print("  ❌  Kokoro TTS not reachable — is Docker running?")
        print("      docker run -p 8880:8880 ghcr.io/remsky/kokoro-fastapi-cpu:v0.2.2")
        sys.exit(1)

    # Check Agenta is reachable
    try:
        requests.get(f"{AGENTA_URL}/health", timeout=3)
        print("  ✅  Agenta daemon: online")
    except requests.RequestException:
        print("  ❌  Agenta daemon not reachable — is it running?")
        print("      agenta daemon start")
        sys.exit(1)

    print()

    # Load wake word model
    print("  Loading wake word model...")
    model = load_wake_word_model()
    print(f"  ✅  Wake word model loaded: {WAKE_WORD_MODEL}")

    # Start listening
    run_wake_word_listener(model)


if __name__ == "__main__":
    main()
