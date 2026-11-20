package kz.qazvoice.download

import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import java.util.Locale
import kotlin.math.min
import kotlin.random.Random

/**
 * Downloads every file of a [JobSpec] into a staging directory, resumably:
 *
 * - partial bytes live in `<name>.part`; the next run continues with `Range` +
 *   `If-Range` (ETag or Last-Modified). A `200` instead of `206` means the server
 *   ignored the range (or the file changed): start that file over.
 * - SHA-256 is computed while streaming (including bytes from earlier runs);
 *   a verified file is renamed to `<name>`, never before.
 * - each mirror gets [maxAttempts] tries with exponential backoff and jitter;
 *   404/403/410 or a wrong checksum move on to the next mirror at once.
 * - with no usable network (or `wifiOnly` on a metered one) it waits instead of
 *   failing, and continues when the [gate] opens.
 * - free space is checked up front and while writing.
 *
 * Pure JVM (no Android APIs) so it is unit-tested against a local HTTP server.
 */
class Downloader(
    private val gate: NetworkGate,
    private val freeBytes: (File) -> Long,
    private val maxAttempts: Int = 5,
    private val baseBackoffMs: Long = 1000,
    private val maxBackoffMs: Long = 30_000,
    private val connectTimeoutMs: Int = 15_000,
    private val readTimeoutMs: Int = 30_000,
    private val progressIntervalMs: Long = 200,
    private val sleeper: (Long, Control) -> Unit = ::interruptibleSleep,
) {
    interface NetworkGate {
        /** Whether a download may run now. */
        fun allowed(wifiOnly: Boolean): Boolean

        /** Blocks until [allowed] or [control] stops; returns false if stopped. */
        fun await(wifiOnly: Boolean, control: Control): Boolean

        /** Why [allowed] is false: `network` or `wifi`. */
        fun reason(wifiOnly: Boolean): String
    }

    interface Listener {
        fun onProgress(received: Long, total: Long, bytesPerSecond: Double)
        fun onWaiting(reason: String, received: Long, total: Long)
        fun onVerifying()
        fun log(message: String) {}
    }

    /** Pause / cancel requests from another thread. */
    class Control {
        @Volatile var stop: Boolean = false
        @Volatile var cancel: Boolean = false
        @Volatile var connection: HttpURLConnection? = null
        private val lock = Object()

        fun requestStop(cancel: Boolean) {
            this.cancel = this.cancel || cancel
            stop = true
            runCatching { connection?.disconnect() }
            synchronized(lock) { lock.notifyAll() }
        }

        fun check() {
            if (stop) throw StoppedException(cancel)
        }

        fun waitFor(ms: Long) {
            synchronized(lock) { if (!stop) lock.wait(ms) }
        }
    }

    private class HttpStatus(val code: Int, msg: String) : IOException(msg)
    private class Restart(msg: String) : IOException(msg)

    /** Bytes already in [staging] for [job] (verified files + partials). */
    fun bytesOnDisk(job: JobSpec, staging: File): Long = job.files.sumOf { f ->
        val done = File(staging, f.name)
        if (done.length() == f.size) f.size else min(File(staging, "${f.name}.part").length(), f.size)
    }

    /**
     * Runs [job] to completion: afterwards every file is in [staging] under its
     * name and verified. Throws [DownloadException] (incl. [StoppedException]).
     */
    fun run(job: JobSpec, staging: File, listener: Listener, control: Control) {
        job.validate()
        staging.mkdirs()
        val total = job.totalBytes
        val verified = job.files.filter { File(staging, it.name).let { d -> d.exists() && d.length() == it.size } }.sumOf { it.size }
        val progress = Progress(total, verified, listener, progressIntervalMs)
        checkSpace(job, staging, bytesOnDisk(job, staging))
        for (f in job.files) {
            control.check()
            val done = File(staging, f.name)
            if (done.length() == f.size && done.exists()) continue
            done.delete()
            fetchWithRetries(job, f, staging, progress, control)
            progress.endFile(f.size)
        }
        listener.onVerifying()
        // Re-check sizes of the verified files (cheap) before reporting success.
        for (f in job.files) {
            val done = File(staging, f.name)
            if (!done.exists() || done.length() != f.size) {
                throw DownloadException("checksum", "${f.name}: size changed on disk", mapOf("file" to f.name))
            }
        }
    }

    private fun checkSpace(job: JobSpec, staging: File, have: Long) {
        val need = job.totalBytes - have + MARGIN
        val free = freeBytes(staging)
        if (free in 0 until need) {
            throw DownloadException("storage", "not enough free storage", mapOf("needed" to need, "available" to free))
        }
    }

    private fun fetchWithRetries(job: JobSpec, f: FileSpec, staging: File, progress: Progress, control: Control) {
        var mirror = 0
        var attempt = 0
        var checksumFailures = 0
        var lastStatus: Int? = null
        var lastError: String = "download failed"
        while (true) {
            control.check()
            if (!gate.allowed(job.wifiOnly)) {
                progress.waiting(gate.reason(job.wifiOnly))
                if (!gate.await(job.wifiOnly, control)) control.check()
                attempt = 0
                continue
            }
            val u = f.urls[mirror]
            try {
                fetch(f, u, staging, progress, control)
                return
            } catch (e: StoppedException) {
                throw e
            } catch (e: DownloadException) {
                if (e.kind != "checksum") throw e
                checksumFailures++
                lastError = e.message ?: "checksum mismatch"
                listener(progress).log("${f.name}: checksum mismatch from ${host(u.url)}")
                // A second mismatch from the same mirror: the mirror has a bad copy.
                if (checksumFailures >= 2) {
                    checksumFailures = 0
                    if (++mirror >= f.urls.size) throw e
                    attempt = 0
                }
                continue
            } catch (e: HttpStatus) {
                lastStatus = e.code
                lastError = e.message ?: "HTTP ${e.code}"
                if (e.code in FATAL_FOR_MIRROR) {
                    listener(progress).log("${f.name}: HTTP ${e.code} from ${host(u.url)}, next mirror")
                    if (++mirror >= f.urls.size) {
                        throw DownloadException("network", lastError, mapOf("status" to lastStatus))
                    }
                    attempt = 0
                    continue
                }
                // 5xx, 429…: retry like a broken connection.
            } catch (e: Restart) {
                lastError = e.message ?: "restart"
                listener(progress).log("${f.name}: ${e.message}; starting the file over")
            } catch (e: IOException) {
                if (control.stop) control.check()
                if (isDiskFull(e)) {
                    throw DownloadException("storage", "storage full", mapOf("needed" to f.size, "available" to freeBytes(staging)), e)
                }
                lastError = "${e.javaClass.simpleName}: ${e.message}"
            }
            control.check()
            if (!gate.allowed(job.wifiOnly)) continue // went offline: wait, don't count
            attempt++
            if (attempt >= maxAttempts) {
                if (++mirror >= f.urls.size) {
                    throw DownloadException("network", lastError, if (lastStatus != null) mapOf("status" to lastStatus) else emptyMap())
                }
                attempt = 0
                continue
            }
            val backoff = min(maxBackoffMs, baseBackoffMs shl (attempt - 1).coerceAtMost(20))
            val jitter = (backoff * 0.2 * Random.nextDouble()).toLong()
            listener(progress).log("${f.name}: $lastError; retry $attempt in ${backoff + jitter} ms")
            sleeper(backoff + jitter, control)
        }
    }

    private fun listener(p: Progress) = p.listener

    private fun fetch(f: FileSpec, u: UrlSpec, staging: File, progress: Progress, control: Control) {
        val part = File(staging, "${f.name}.part")
        val meta = File(staging, "${f.name}.meta")
        var have = part.length()
        if (have > f.size) {
            part.delete(); meta.delete(); have = 0
        }
        // Without a validator we still resume: the SHA-256 check catches a file
        // that changed on the server, and the next attempt starts over.
        val validator = if (have > 0) readMeta(meta) else null
        val md = MessageDigest.getInstance("SHA-256")
        if (have > 0) hashFile(part, have, md, control)
        progress.beginFile(have)
        if (have == f.size && f.size > 0) {
            finishFile(f, part, meta, staging, md)
            return
        }
        val conn = open(u, if (have > 0) have else null, validator, control)
        try {
            val code = conn.responseCode
            val append: Boolean
            when {
                code == 206 && have > 0 -> {
                    val start = parseContentRangeStart(conn.getHeaderField("Content-Range"))
                    if (start != have) {
                        part.delete(); meta.delete()
                        throw Restart("server resumed at $start, expected $have")
                    }
                    append = true
                }
                code == 200 || code == 206 -> {
                    if (have > 0) listener(progress).log("${f.name}: server sent the whole file (HTTP $code), restarting")
                    md.reset()
                    have = 0
                    append = false
                }
                code == 416 -> {
                    part.delete(); meta.delete()
                    throw Restart("range not satisfiable")
                }
                else -> throw HttpStatus(code, "HTTP $code from ${host(conn.url.toString())}")
            }
            val len = conn.contentLengthLong
            if (len >= 0 && have + len != f.size) {
                part.delete(); meta.delete()
                throw DownloadException("checksum", "${f.name}: server size ${have + len} ≠ ${f.size}", mapOf("file" to f.name))
            }
            writeMeta(meta, conn.getHeaderField("ETag"), conn.getHeaderField("Last-Modified"))
            progress.beginFile(have)
            FileOutputStream(part, append).use { out ->
                val input = conn.inputStream
                val buf = ByteArray(BUF)
                var written = have
                var sinceSpaceCheck = 0L
                while (true) {
                    control.check()
                    val n = input.read(buf)
                    if (n < 0) break
                    if (written + n > f.size) {
                        part.delete(); meta.delete()
                        throw DownloadException("checksum", "${f.name}: more bytes than expected", mapOf("file" to f.name))
                    }
                    out.write(buf, 0, n)
                    md.update(buf, 0, n)
                    written += n
                    sinceSpaceCheck += n
                    if (sinceSpaceCheck >= SPACE_CHECK_EVERY) {
                        sinceSpaceCheck = 0
                        val free = freeBytes(staging)
                        if (free in 0 until MIN_FREE) {
                            throw DownloadException("storage", "storage full", mapOf("needed" to (f.size - written + MARGIN), "available" to free))
                        }
                    }
                    progress.add(n.toLong())
                }
                out.fd.sync()
                if (written != f.size) throw IOException("connection closed at $written of ${f.size} bytes")
            }
        } finally {
            control.connection = null
            conn.disconnect()
        }
        finishFile(f, part, meta, staging, md)
    }

    private fun finishFile(f: FileSpec, part: File, meta: File, staging: File, md: MessageDigest) {
        val got = md.digest().joinToString("") { "%02x".format(it) }
        if (got != f.sha256) {
            part.delete(); meta.delete()
            throw DownloadException("checksum", "${f.name}: SHA-256 mismatch", mapOf("file" to f.name))
        }
        val done = File(staging, f.name)
        if (!part.renameTo(done)) throw IOException("cannot rename ${part.name}")
        meta.delete()
    }

    /** Opens [u], following redirects by hand so Range survives and the mirror's
     *  headers are not sent to another host. */
    private fun open(u: UrlSpec, from: Long?, validator: String?, control: Control): HttpURLConnection {
        var url = URL(u.url)
        val origin = url.host.lowercase(Locale.ROOT)
        repeat(MAX_REDIRECTS) {
            control.check()
            val c = url.openConnection() as HttpURLConnection
            c.instanceFollowRedirects = false
            c.connectTimeout = connectTimeoutMs
            c.readTimeout = readTimeoutMs
            c.setRequestProperty("User-Agent", "qazvoice-downloader")
            c.setRequestProperty("Accept-Encoding", "identity")
            if (url.host.lowercase(Locale.ROOT) == origin) {
                for ((k, v) in u.headers) c.setRequestProperty(k, v)
            }
            if (from != null) {
                c.setRequestProperty("Range", "bytes=$from-")
                if (validator != null) c.setRequestProperty("If-Range", validator)
            }
            control.connection = c
            val code = try {
                c.responseCode
            } catch (e: IOException) {
                control.connection = null
                c.disconnect()
                throw e
            }
            if (code in 300..399 && code != 304) {
                val loc = c.getHeaderField("Location")
                c.disconnect()
                if (loc.isNullOrEmpty()) throw HttpStatus(code, "redirect without Location")
                val next = URL(url, loc)
                val scheme = next.protocol.lowercase(Locale.ROOT)
                if (scheme != "https" && scheme != "http") throw HttpStatus(400, "redirect to $scheme")
                if (url.protocol == "https" && scheme == "http") throw HttpStatus(400, "redirect from https to http")
                url = next
                return@repeat
            }
            return c
        }
        throw HttpStatus(310, "too many redirects")
    }

    private class Progress(val total: Long, var base: Long, val listener: Listener, val interval: Long) {
        private var current = 0L
        private var last = 0L
        private var speed = 0.0
        private var windowStart = System.nanoTime()
        private var windowBytes = 0L
        val received get() = (base + current).coerceAtMost(total)

        /** The file being fetched now has [have] bytes on disk. */
        fun beginFile(have: Long) {
            current = have
            emit(force = true)
        }

        /** The current file of [size] bytes is verified. */
        fun endFile(size: Long) {
            base += size
            current = 0
        }

        fun add(n: Long) {
            current += n
            windowBytes += n
            emit(force = false)
        }

        fun waiting(reason: String) {
            speed = 0.0
            windowBytes = 0
            windowStart = System.nanoTime()
            listener.onWaiting(reason, received, total)
        }

        private fun emit(force: Boolean) {
            val now = System.nanoTime()
            val dt = (now - windowStart) / 1e9
            if (dt >= 1.0) {
                val inst = windowBytes / dt
                speed = if (speed == 0.0) inst else speed * 0.6 + inst * 0.4
                windowStart = now
                windowBytes = 0
            }
            if (force || (now - last) / 1_000_000 >= interval) {
                last = now
                listener.onProgress(received, total, speed)
            }
        }
    }

    companion object {
        private const val BUF = 256 * 1024
        private const val MAX_REDIRECTS = 8
        const val MARGIN = 16L * 1024 * 1024
        private const val MIN_FREE = 4L * 1024 * 1024
        private const val SPACE_CHECK_EVERY = 8L * 1024 * 1024
        private val FATAL_FOR_MIRROR = setOf(400, 401, 403, 404, 410, 451)

        fun host(url: String) = runCatching { URL(url).host }.getOrDefault("?")

        fun interruptibleSleep(ms: Long, control: Control) {
            val end = System.currentTimeMillis() + ms
            while (!control.stop) {
                val left = end - System.currentTimeMillis()
                if (left <= 0) return
                control.waitFor(left)
            }
        }

        fun parseContentRangeStart(h: String?): Long? =
            h?.let { Regex("bytes\\s+(\\d+)-").find(it)?.groupValues?.get(1)?.toLongOrNull() }

        fun isDiskFull(e: Throwable): Boolean {
            var t: Throwable? = e
            while (t != null) {
                val m = t.message ?: ""
                if (m.contains("ENOSPC") || m.contains("No space left", ignoreCase = true)) return true
                t = t.cause
            }
            return false
        }

        private fun hashFile(f: File, len: Long, md: MessageDigest, control: Control) {
            FileInputStream(f).use { i ->
                val b = ByteArray(BUF)
                var left = len
                while (left > 0) {
                    control.check()
                    val n = i.read(b, 0, min(b.size.toLong(), left).toInt())
                    if (n < 0) break
                    md.update(b, 0, n)
                    left -= n
                }
            }
        }

        private fun readMeta(f: File): String? = runCatching {
            f.readLines().firstOrNull { it.isNotBlank() }?.trim()
        }.getOrNull()

        /** Prefers a strong ETag (weak ones are not valid in If-Range), else Last-Modified. */
        private fun writeMeta(f: File, etag: String?, lastModified: String?) {
            val v = when {
                etag != null && !etag.startsWith("W/") -> etag
                lastModified != null -> lastModified
                else -> null
            }
            if (v == null) f.delete() else f.writeText(v)
        }
    }
}
