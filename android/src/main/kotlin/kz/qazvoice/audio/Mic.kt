package kz.qazvoice.audio

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder

/**
 * Microphone capture for recognition: VOICE_RECOGNITION source (no extra noise
 * suppression), 16-bit mono, delivered in 100 ms chunks on a background thread.
 */
class Mic(private val rate: Int, private val onChunk: (ByteArray) -> Unit, private val onError: (String) -> Unit) {
    @Volatile private var running = false
    private var thread: Thread? = null

    @SuppressLint("MissingPermission") // checked by the plugin before start()
    fun start() {
        val minBuf = AudioRecord.getMinBufferSize(rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuf <= 0) {
            onError("sample rate $rate not supported by the microphone")
            return
        }
        val rec = try {
            AudioRecord(MediaRecorder.AudioSource.VOICE_RECOGNITION, rate, AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT, maxOf(minBuf, rate * 2))
        } catch (e: Exception) {
            onError("microphone unavailable: ${e.message}")
            return
        }
        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            rec.release()
            onError("microphone unavailable (busy or no permission)")
            return
        }
        running = true
        thread = Thread({
            val frame = ByteArray(rate / 10 * 2)
            try {
                rec.startRecording()
                while (running) {
                    var off = 0
                    while (running && off < frame.size) {
                        val n = rec.read(frame, off, frame.size - off)
                        if (n < 0) {
                            onError("AudioRecord.read error $n")
                            running = false
                            break
                        }
                        off += n
                    }
                    if (off > 0 && running) onChunk(frame.copyOf(off))
                }
            } catch (e: Exception) {
                onError("microphone failed: ${e.message}")
            } finally {
                runCatching { rec.stop() }
                rec.release()
            }
        }, "qazvoice-mic").apply { isDaemon = true; start() }
    }

    fun stop() {
        running = false
        thread?.join(200)
        thread = null
    }
}
