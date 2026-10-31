"""Tiến trình tạo giọng đọc VieNeu-TTS trên máy, do ReadComments.exe khởi động.

Giao thức: mỗi yêu cầu là một dòng JSON {"text": ...} trên stdin; mỗi trả lời là một
dòng JSON trên stdout, {"size": N} kèm N byte WAV ngay sau đó, hoặc {"error": ...}.
Đóng stdin thì tiến trình thoát.
"""
import io
import json
import os
import sys
import wave

# Dừng tải mô hình sau một lần chạy thành công nhờ HF_HUB_OFFLINE do chương trình chính đặt
CPU_THREADS = 2
MODELS = {
    "nano": {"mode": "v3nano", "threads": CPU_THREADS},
    "turbo": {"mode": "v3turbo", "backend": "onnx", "precision": "int8", "threads": CPU_THREADS},
}


def wav_bytes(audio, sample_rate):
    import numpy as np

    samples = (np.clip(audio, -1.0, 1.0) * 32767).astype(np.int16)
    buffer = io.BytesIO()
    with wave.open(buffer, "wb") as output:
        output.setnchannels(1)
        output.setsampwidth(2)
        output.setframerate(sample_rate)
        output.writeframes(samples.tobytes())
    return buffer.getvalue()


def main():
    model, requested_voice = sys.argv[1], sys.argv[2]

    # Thư viện có thể in ra stdout, nên giữ riêng kênh trả lời rồi chuyển stdout sang stderr
    reply = os.fdopen(os.dup(1), "wb")
    os.dup2(2, 1)

    def send(header, data=b""):
        reply.write(json.dumps(header).encode("utf-8") + b"\n" + data)
        reply.flush()

    from vieneu import Vieneu

    tts = Vieneu(**MODELS[model])
    voices = [voice_id for _, voice_id in tts.list_preset_voices()]
    voice = requested_voice if requested_voice in voices else voices[0]
    send({"ready": True, "voice": voice})

    for line in sys.stdin.buffer:
        try:
            audio = tts.infer(json.loads(line)["text"], voice=voice)
            if len(audio) == 0:
                send({"error": "VieNeu không tạo ra âm thanh"})
                continue
            data = wav_bytes(audio, tts.sample_rate)
        except Exception as error:  # một câu lỗi không được làm dừng cả tiến trình
            send({"error": str(error)[:300]})
            continue
        send({"size": len(data)}, data)


if __name__ == "__main__":
    main()
