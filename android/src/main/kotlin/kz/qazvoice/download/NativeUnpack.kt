package kz.qazvoice.download

import java.io.File
import java.io.IOException

/** zstd decompression in libqazvoice (Rust), called on the download thread. */
object NativeUnpack {
    @Volatile private var loaded = false

    @JvmStatic private external fun unzstd(src: String, dst: String, maxOut: Long): String

    /** Returns (bytes written, SHA-256 hex of the output). */
    fun unzstdFile(src: File, dst: File, maxOut: Long): Pair<Long, String> {
        if (!loaded) {
            System.loadLibrary("qazvoice")
            loaded = true
        }
        val r = unzstd(src.path, dst.path, maxOut)
        if (r.startsWith("error:")) throw IOException(r.removePrefix("error:").trim())
        val (n, sha) = r.split(' ', limit = 2)
        return n.toLong() to sha
    }
}
