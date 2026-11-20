package kz.qazvoice.audio

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.MediaMuxer
import java.io.File
import java.io.FileInputStream
import java.io.IOException

/** Raw 16-bit mono PCM → AAC-LC in an .m4a (MPEG-4) file. */
object M4aEncoder {
    fun encode(input: File, rate: Int, output: File, bitrate: Int = 64_000) {
        val fmt = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, rate, 1).apply {
            setInteger(MediaFormat.KEY_AAC_PROFILE, MediaCodecInfo.CodecProfileLevel.AACObjectLC)
            setInteger(MediaFormat.KEY_BIT_RATE, bitrate)
            setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 16384)
        }
        val codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_AAC)
        val tmp = File(output.path + ".tmp")
        var muxer: MediaMuxer? = null
        try {
            codec.configure(fmt, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            codec.start()
            muxer = MediaMuxer(tmp.path, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4)
            var track = -1
            val info = MediaCodec.BufferInfo()
            var inputDone = false
            var outputDone = false
            var samplesIn = 0L
            FileInputStream(input).use { src ->
                val buf = ByteArray(8192)
                while (!outputDone) {
                    if (!inputDone) {
                        val idx = codec.dequeueInputBuffer(10_000)
                        if (idx >= 0) {
                            val ib = codec.getInputBuffer(idx)!!
                            ib.clear()
                            val n = src.read(buf, 0, minOf(buf.size, ib.remaining()) and 1.inv())
                            val pts = samplesIn * 1_000_000L / rate
                            if (n <= 0) {
                                codec.queueInputBuffer(idx, 0, 0, pts, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
                                inputDone = true
                            } else {
                                ib.put(buf, 0, n)
                                codec.queueInputBuffer(idx, 0, n, pts, 0)
                                samplesIn += n / 2
                            }
                        }
                    }
                    val out = codec.dequeueOutputBuffer(info, 10_000)
                    when {
                        out == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                            track = muxer.addTrack(codec.outputFormat)
                            muxer.start()
                        }
                        out >= 0 -> {
                            val ob = codec.getOutputBuffer(out)!!
                            if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) info.size = 0
                            if (info.size > 0 && track >= 0) {
                                ob.position(info.offset)
                                ob.limit(info.offset + info.size)
                                muxer.writeSampleData(track, ob, info)
                            }
                            codec.releaseOutputBuffer(out, false)
                            if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) outputDone = true
                        }
                    }
                }
            }
            if (track < 0) throw IOException("encoder produced no output")
            muxer.stop()
            muxer.release()
            muxer = null
            if (!tmp.renameTo(output)) throw IOException("cannot write ${output.path}")
        } finally {
            runCatching { muxer?.release() }
            runCatching { codec.stop() }
            codec.release()
            tmp.delete()
        }
    }
}
