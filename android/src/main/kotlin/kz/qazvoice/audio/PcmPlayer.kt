package kz.qazvoice.audio

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.os.Build
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import kotlin.math.abs

/**
 * Streams 16-bit mono PCM to an AudioTrack (ported from QazSTT-Online's TtsPlayer).
 * [write] queues a chunk and calls back once fewer than [maxQueuedMs] of audio
 * wait (back-pressure for the synthesizer); [finish] calls back when everything
 * has been heard. Reports the playback position and level every ~50 ms.
 */
class PcmPlayer(
    ctx: Context,
    private val rate: Int,
    private val onPosition: (frames: Long, level: Float) -> Unit,
    private val maxQueuedMs: Int = 3000,
) {
    private val queue = LinkedBlockingQueue<ByteArray>()
    @Volatile private var stopped = false
    @Volatile private var ended = false
    private val lock = Object()
    private var queuedBytes = 0L
    private var writtenFrames = 0L
    private val levels = ArrayList<Float>() // per 512-frame block
    private val waiters = ArrayList<Pair<Long, () -> Unit>>() // (threshold bytes, callback)
    private var finishCallback: (() -> Unit)? = null
    private val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
    private var focus: AudioFocusRequest? = null

    private val attrs = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_MEDIA)
        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
        .build()

    private val track: AudioTrack = AudioTrack.Builder()
        .setAudioAttributes(attrs)
        .setAudioFormat(AudioFormat.Builder().setSampleRate(rate).setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
            .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
        .setBufferSizeInBytes(maxOf(AudioTrack.getMinBufferSize(rate, AudioFormat.CHANNEL_OUT_MONO, AudioFormat.ENCODING_PCM_16BIT), rate / 5 * 2))
        .setTransferMode(AudioTrack.MODE_STREAM)
        .build()

    private val thread = Thread({ loop() }, "qazvoice-player").apply { isDaemon = true }

    init {
        requestFocus()
        thread.start()
    }

    private fun requestFocus() {
        if (Build.VERSION.SDK_INT >= 26) {
            val r = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK).setAudioAttributes(attrs).build()
            focus = r
            runCatching { am.requestAudioFocus(r) }
        }
    }

    private fun abandonFocus() {
        if (Build.VERSION.SDK_INT >= 26) focus?.let { runCatching { am.abandonAudioFocusRequest(it) } }
    }

    private val slice = maxOf(512, rate / 20 * 2)

    private val limitBytes get() = rate.toLong() * 2 * maxQueuedMs / 1000

    fun write(pcm: ByteArray, done: () -> Unit) {
        if (stopped) { done(); return }
        synchronized(lock) {
            queuedBytes += pcm.size
            queue.put(pcm)
            if (queuedBytes <= limitBytes) done() else waiters += (limitBytes to done)
        }
    }

    fun finish(done: () -> Unit) {
        synchronized(lock) {
            if (stopped) { done(); return }
            finishCallback = done
            ended = true
        }
        queue.put(ByteArray(0)) // wake the loop
    }

    fun stop() {
        stopped = true
        runCatching { track.pause(); track.flush() }
        queue.put(ByteArray(0))
        releaseWaiters()
    }

    private fun releaseWaiters() {
        val ws: List<() -> Unit>
        val fin: (() -> Unit)?
        synchronized(lock) {
            ws = waiters.map { it.second }
            waiters.clear()
            fin = if (stopped) finishCallback.also { finishCallback = null } else null
        }
        ws.forEach { it() }
        fin?.invoke()
    }

    private fun loop() {
        var lastReport = 0L
        try {
            track.play()
            while (!stopped) {
                val chunk = queue.poll(50, TimeUnit.MILLISECONDS)
                if (chunk != null && chunk.isNotEmpty()) {
                    synchronized(lock) {
                        var i = 0
                        while (i < chunk.size - 1) {
                            var peak = 0
                            var j = i
                            val end = minOf(chunk.size - 1, i + 1024)
                            while (j < end) {
                                val v = (chunk[j].toInt() and 0xff) or (chunk[j + 1].toInt() shl 8)
                                val a = abs(v.toShort().toInt())
                                if (a > peak) peak = a
                                j += 8
                            }
                            levels += (peak / 32768f)
                            i += 1024
                        }
                    }
                    var off = 0
                    while (off < chunk.size && !stopped) {
                        // ~50 ms slices: write() blocks while the buffer is full, and
                        // position/level reports happen between slices.
                        val w = track.write(chunk, off, minOf(chunk.size - off, slice))
                        if (w < 0) { stopped = true; break }
                        off += w
                        report(force = false, lastReport).also { lastReport = it }
                    }
                    val waiting: List<() -> Unit>
                    synchronized(lock) {
                        queuedBytes -= chunk.size
                        writtenFrames += chunk.size / 2
                        waiting = waiters.filter { queuedBytes <= it.first }.map { it.second }
                        waiters.removeAll { queuedBytes <= it.first }
                    }
                    waiting.forEach { it() }
                }
                lastReport = report(force = false, lastReport)
                if (ended && queue.isEmpty()) {
                    // Let the buffered tail play out.
                    while (!stopped && track.playbackHeadPosition.toLong() and 0xffffffffL < writtenFrames) {
                        Thread.sleep(20)
                        lastReport = report(force = false, lastReport)
                    }
                    break
                }
            }
        } catch (_: InterruptedException) {
        } finally {
            onPosition(writtenFrames, 0f)
            runCatching { track.stop() }
            track.release()
            abandonFocus()
            val fin: (() -> Unit)?
            synchronized(lock) { fin = finishCallback; finishCallback = null }
            releaseWaiters()
            fin?.invoke()
        }
    }

    private fun report(force: Boolean, last: Long): Long {
        val now = System.nanoTime()
        if (!force && now - last < 50_000_000) return last
        val head = track.playbackHeadPosition.toLong() and 0xffffffffL
        val level = synchronized(lock) { levels.getOrNull((head / 512).toInt()) ?: 0f }
        onPosition(head, level)
        return now
    }
}
