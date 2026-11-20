package kz.qazvoice.audio

import android.content.Context
import android.media.AudioFormat
import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.net.Uri
import java.io.BufferedOutputStream
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Decodes any audio the platform supports (wav, mp3, m4a/aac, ogg/opus, flac,
 * amr, the soundtrack of mp4/mkv/webm video…) to interleaved 16-bit
 * little-endian PCM in a file, at the source's own rate and channel count
 * (the engine resamples). Ported from QazSTT-Online's AudioDecoder.
 */
object AudioDecoder {
    class Result(val rate: Int, val channels: Int, val durationUs: Long, val bytes: Long)

    fun decode(
        ctx: Context,
        input: String,
        output: File,
        cancelled: () -> Boolean,
        onProgress: (Double) -> Unit,
    ): Result {
        val extractor = MediaExtractor()
        try {
            if (input.startsWith("content://") || input.startsWith("file://")) {
                extractor.setDataSource(ctx, Uri.parse(input), null)
            } else {
                extractor.setDataSource(input)
            }
        } catch (e: Exception) {
            extractor.release()
            throw IOException("cannot open $input: ${e.message}")
        }
        var codec: MediaCodec? = null
        try {
            val track = (0 until extractor.trackCount).firstOrNull {
                extractor.getTrackFormat(it).getString(MediaFormat.KEY_MIME)?.startsWith("audio/") == true
            } ?: throw IOException("no audio track")
            extractor.selectTrack(track)
            val inFmt = extractor.getTrackFormat(track)
            val mime = inFmt.getString(MediaFormat.KEY_MIME)!!
            var rate = inFmt.getInteger(MediaFormat.KEY_SAMPLE_RATE)
            var channels = inFmt.getInteger(MediaFormat.KEY_CHANNEL_COUNT)
            val durationUs = if (inFmt.containsKey(MediaFormat.KEY_DURATION)) inFmt.getLong(MediaFormat.KEY_DURATION) else 0L
            val c = MediaCodec.createDecoderByType(mime)
            codec = c
            c.configure(inFmt, null, null, 0)
            c.start()
            var pcmEncoding = AudioFormat.ENCODING_PCM_16BIT
            var formatLocked = false
            val info = MediaCodec.BufferInfo()
            var inputDone = false
            var outputDone = false
            var bytes = 0L
            var lastProgress = 0L
            BufferedOutputStream(FileOutputStream(output), 1 shl 16).use { out ->
                val tmp = ByteArray(1 shl 16)
                while (!outputDone) {
                    if (cancelled()) throw CancelledException()
                    if (!inputDone) {
                        val inIdx = c.dequeueInputBuffer(10_000)
                        if (inIdx >= 0) {
                            val buf = c.getInputBuffer(inIdx)!!
                            val n = extractor.readSampleData(buf, 0)
                            if (n < 0) {
                                c.queueInputBuffer(inIdx, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
                                inputDone = true
                            } else {
                                c.queueInputBuffer(inIdx, 0, n, extractor.sampleTime, 0)
                                extractor.advance()
                            }
                        }
                    }
                    val outIdx = c.dequeueOutputBuffer(info, 10_000)
                    when {
                        outIdx == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                            val f = c.outputFormat
                            if (!formatLocked) {
                                rate = f.getInteger(MediaFormat.KEY_SAMPLE_RATE)
                                channels = f.getInteger(MediaFormat.KEY_CHANNEL_COUNT)
                            }
                            if (f.containsKey(MediaFormat.KEY_PCM_ENCODING)) pcmEncoding = f.getInteger(MediaFormat.KEY_PCM_ENCODING)
                        }
                        outIdx >= 0 -> {
                            if (info.size > 0) {
                                formatLocked = true
                                val ob = c.getOutputBuffer(outIdx)!!.order(ByteOrder.LITTLE_ENDIAN)
                                ob.position(info.offset)
                                ob.limit(info.offset + info.size)
                                bytes += writePcm(ob, pcmEncoding, out, tmp)
                                if (durationUs > 0 && info.presentationTimeUs - lastProgress > 500_000) {
                                    lastProgress = info.presentationTimeUs
                                    onProgress((info.presentationTimeUs.toDouble() / durationUs).coerceIn(0.0, 1.0))
                                }
                            }
                            c.releaseOutputBuffer(outIdx, false)
                            if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) outputDone = true
                        }
                    }
                }
            }
            onProgress(1.0)
            if (bytes == 0L) throw IOException("no audio decoded")
            return Result(rate, channels, durationUs, bytes)
        } catch (e: Exception) {
            output.delete()
            throw e
        } finally {
            runCatching { codec?.stop() }
            runCatching { codec?.release() }
            extractor.release()
        }
    }

    private fun writePcm(b: ByteBuffer, encoding: Int, out: BufferedOutputStream, tmp: ByteArray): Long {
        var written = 0L
        when (encoding) {
            AudioFormat.ENCODING_PCM_FLOAT -> {
                val fb = b.asFloatBuffer()
                var i = 0
                while (fb.hasRemaining()) {
                    val s = (fb.get().coerceIn(-1f, 1f) * 32767f).toInt()
                    tmp[i++] = s.toByte()
                    tmp[i++] = (s shr 8).toByte()
                    if (i == tmp.size) { out.write(tmp, 0, i); written += i; i = 0 }
                }
                out.write(tmp, 0, i); written += i
            }
            AudioFormat.ENCODING_PCM_8BIT -> {
                var i = 0
                while (b.hasRemaining()) {
                    val s = ((b.get().toInt() and 0xff) - 128) shl 8
                    tmp[i++] = s.toByte()
                    tmp[i++] = (s shr 8).toByte()
                    if (i == tmp.size) { out.write(tmp, 0, i); written += i; i = 0 }
                }
                out.write(tmp, 0, i); written += i
            }
            else -> {
                while (b.hasRemaining()) {
                    val n = minOf(tmp.size, b.remaining())
                    b.get(tmp, 0, n)
                    out.write(tmp, 0, n)
                    written += n
                }
            }
        }
        return written
    }

    class CancelledException : IOException("cancelled")
}
