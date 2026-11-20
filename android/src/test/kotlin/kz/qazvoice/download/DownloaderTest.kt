package kz.qazvoice.download

import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import java.io.File
import java.nio.file.Files
import java.security.MessageDigest
import java.util.Collections
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlin.random.Random

class DownloaderTest {
    private lateinit var server: TinyHttpServer
    private lateinit var dir: File
    private val requests = Collections.synchronizedList(mutableListOf<String>()) // "path range auth"
    private val behaviors = HashMap<String, Behavior>()
    private var port = 0

    private val dataA = Random(1).nextBytes(3 * 1024 * 1024 + 123)
    private val dataB = Random(2).nextBytes(700_001)

    /** How the test server answers one path. */
    class Behavior(
        var data: ByteArray,
        var etag: String? = "\"v1\"",
        var status: Int = 0, // 0 = normal
        var dropAfter: Long = -1, // close the connection after this many body bytes (once)
        var ignoreRange: Boolean = false,
        var bytesPerSecond: Long = 0, // 0 = unthrottled
        var corrupt: Boolean = false,
        var redirectTo: String? = null,
        var lengthOverride: Long = -1,
    ) {
        val dropped = AtomicBoolean(false)
    }

    @Before
    fun setUp() {
        dir = Files.createTempDirectory("qazvoice-dl").toFile()
        server = TinyHttpServer { ex -> handle(ex) }
        port = server.port
    }

    @After
    fun tearDown() {
        server.stop()
        dir.deleteRecursively()
    }

    private fun handle(ex: TinyHttpServer.Exchange) {
        val path = ex.path
        val range = ex.header("Range")
        val ifRange = ex.header("If-Range")
        val auth = ex.header("Authorization")
        requests += "$path ${range ?: "-"} ${auth ?: "-"} ${ex.header("Host")}"
        val b = behaviors[path]
        if (b == null) {
            ex.respond(404, length = 0)
            return
        }
        b.redirectTo?.let {
            ex.respond(302, mapOf("Location" to it), length = 0)
            return
        }
        if (b.status != 0) {
            ex.respond(b.status, length = 0)
            return
        }
        var start = 0L
        var code = 200
        val h = LinkedHashMap<String, String>()
        if (range != null && !b.ignoreRange && (ifRange == null || ifRange == b.etag)) {
            start = Regex("bytes=(\\d+)-").find(range)!!.groupValues[1].toLong()
            if (start >= b.data.size) {
                ex.respond(416, length = 0)
                return
            }
            code = 206
            h["Content-Range"] = "bytes $start-${b.data.size - 1}/${b.data.size}"
        }
        b.etag?.let { h["ETag"] = it }
        val body = b.data.copyOfRange(start.toInt(), b.data.size)
        if (b.corrupt && body.isNotEmpty()) body[body.size / 2] = (body[body.size / 2] + 1).toByte()
        ex.respond(code, h, if (b.lengthOverride >= 0) b.lengthOverride else body.size.toLong())
        var sent = 0L
        val chunk = 16 * 1024
        var off = 0
        while (off < body.size) {
            val n = minOf(chunk, body.size - off)
            if (b.dropAfter >= 0 && !b.dropped.get() && sent + n > b.dropAfter) {
                b.dropped.set(true)
                ex.out.write(body, off, (b.dropAfter - sent).toInt())
                ex.out.flush()
                ex.close() // abrupt close mid-body
                return
            }
            ex.out.write(body, off, n)
            off += n
            sent += n
            if (b.bytesPerSecond > 0) Thread.sleep(n * 1000 / b.bytesPerSecond)
        }
        ex.out.flush()
    }

    private fun url(path: String, host: String = "127.0.0.1") = "http://$host:$port$path"

    private fun sha(b: ByteArray) = MessageDigest.getInstance("SHA-256").digest(b).joinToString("") { "%02x".format(it) }

    private fun job(vararg files: FileSpec, wifiOnly: Boolean = false) = JobSpec("tts-kk", 1, "Voice", wifiOnly, files.toList())

    private fun file(name: String, data: ByteArray, vararg urls: UrlSpec) = FileSpec(name, data.size.toLong(), sha(data), urls.toList())

    private class Gate(@Volatile var online: Boolean = true) : Downloader.NetworkGate {
        override fun allowed(wifiOnly: Boolean) = online
        override fun reason(wifiOnly: Boolean) = "network"
        override fun await(wifiOnly: Boolean, control: Downloader.Control): Boolean {
            while (!control.stop) {
                if (online) return true
                Thread.sleep(10)
            }
            return false
        }
    }

    private class Events : Downloader.Listener {
        val progress = Collections.synchronizedList(mutableListOf<Long>())
        val waiting = AtomicInteger()
        val logs = Collections.synchronizedList(mutableListOf<String>())
        var verifying = false
        override fun onProgress(received: Long, total: Long, bytesPerSecond: Double) { progress += received }
        override fun onWaiting(reason: String, received: Long, total: Long) { waiting.incrementAndGet() }
        override fun onVerifying() { verifying = true }
        override fun log(message: String) { logs += message }
    }

    /** Test "zstd": the packed bytes are the original reversed. */
    private val fakeUnzstd: (File, File, Long) -> Pair<Long, String> = { src, dst, _ ->
        val out = src.readBytes().reversedArray()
        dst.writeBytes(out)
        out.size.toLong() to sha(out)
    }

    private fun downloader(
        gate: Downloader.NetworkGate = Gate(),
        free: (File) -> Long = { Long.MAX_VALUE / 4 },
        attempts: Int = 4,
        unzstd: (File, File, Long) -> Pair<Long, String> = fakeUnzstd,
    ) = Downloader(gate, free, maxAttempts = attempts, baseBackoffMs = 5, maxBackoffMs = 20, readTimeoutMs = 2000, progressIntervalMs = 0, unzstd = unzstd)

    private fun zfile(name: String, data: ByteArray, packedPath: String) = FileSpec(
        name, data.size.toLong(), sha(data), listOf(UrlSpec(url("/raw-not-used"))),
        ZstSpec(data.size.toLong(), sha(data.reversedArray()), listOf(UrlSpec(url(packedPath)))))

    @Test
    fun compressedFileIsDownloadedUnpackedAndVerified() {
        behaviors["/a.zst"] = Behavior(dataA.reversedArray(), dropAfter = 900_000)
        val ev = Events()
        downloader().run(job(zfile("a.onnx", dataA, "/a.zst")), staging(), ev, Downloader.Control())
        assertArrayEquals(dataA, File(staging(), "a.onnx").readBytes())
        assertFalse(File(staging(), "a.onnx.zst").exists())
        assertFalse(File(staging(), "a.onnx.unpack").exists())
        assertTrue(requests.none { it.startsWith("/raw-not-used") })
        assertTrue(requests.any { it.startsWith("/a.zst bytes=") }) // resumed after the drop
        assertTrue(ev.verifying)
    }

    @Test
    fun badUnpackResultRefetchesOnceThenFails() {
        behaviors["/a.zst"] = Behavior(dataB.reversedArray())
        var calls = 0
        val wrong: (File, File, Long) -> Pair<Long, String> = { _, dst, _ -> calls++; dst.writeBytes(byteArrayOf(1)); 1L to "0".repeat(64) }
        try {
            downloader(unzstd = wrong).run(job(zfile("b.json", dataB, "/a.zst")), staging(), Events(), Downloader.Control())
            fail()
        } catch (e: DownloadException) {
            assertEquals("checksum", e.kind)
        }
        assertEquals(2, calls)
        assertEquals(2, requests.count { it.startsWith("/a.zst") })
        assertFalse(File(staging(), "b.json").exists())
        assertFalse(File(staging(), "b.json.unpack").exists())
    }

    @Test
    fun downloadedButNotUnpackedIsNotFetchedAgain() {
        behaviors["/a.zst"] = Behavior(dataB.reversedArray())
        staging().mkdirs()
        File(staging(), "b.json.zst").writeBytes(dataB.reversedArray())
        val j = job(zfile("b.json", dataB, "/a.zst"))
        assertEquals(dataB.size.toLong(), downloader().bytesOnDisk(j, staging()))
        downloader().run(j, staging(), Events(), Downloader.Control())
        assertTrue(requests.isEmpty())
        assertArrayEquals(dataB, File(staging(), "b.json").readBytes())
    }

    private fun staging() = File(dir, "staging")

    @Test
    fun downloadsAndVerifiesAllFiles() {
        behaviors["/a"] = Behavior(dataA)
        behaviors["/b"] = Behavior(dataB)
        val ev = Events()
        downloader().run(job(file("a.onnx", dataA, UrlSpec(url("/a"))), file("b.json", dataB, UrlSpec(url("/b")))), staging(), ev, Downloader.Control())
        assertArrayEquals(dataA, File(staging(), "a.onnx").readBytes())
        assertArrayEquals(dataB, File(staging(), "b.json").readBytes())
        assertFalse(File(staging(), "a.onnx.part").exists())
        assertTrue(ev.verifying)
        assertEquals((dataA.size + dataB.size).toLong(), ev.progress.last())
        // Progress never goes backwards on a clean run.
        assertEquals(ev.progress, ev.progress.sorted())
    }

    @Test
    fun resumesAfterConnectionDropWithRange() {
        behaviors["/a"] = Behavior(dataA, dropAfter = 1_000_000)
        downloader().run(job(file("a.onnx", dataA, UrlSpec(url("/a")))), staging(), Events(), Downloader.Control())
        assertArrayEquals(dataA, File(staging(), "a.onnx").readBytes())
        assertTrue(requests.toString(), requests.any { it.startsWith("/a bytes=") })
    }

    @Test
    fun restartsWhenServerIgnoresRange() {
        behaviors["/a"] = Behavior(dataA, dropAfter = 500_000, ignoreRange = true)
        val ev = Events()
        downloader().run(job(file("a.onnx", dataA, UrlSpec(url("/a")))), staging(), ev, Downloader.Control())
        assertArrayEquals(dataA, File(staging(), "a.onnx").readBytes())
        assertTrue(ev.logs.toString(), ev.logs.any { it.contains("whole file") })
    }

    @Test
    fun wrongChecksumFailsOverToNextMirror() {
        behaviors["/bad"] = Behavior(dataA, corrupt = true)
        behaviors["/good"] = Behavior(dataA)
        downloader().run(job(file("a.onnx", dataA, UrlSpec(url("/bad")), UrlSpec(url("/good")))), staging(), Events(), Downloader.Control())
        assertArrayEquals(dataA, File(staging(), "a.onnx").readBytes())
        assertEquals(2, requests.count { it.startsWith("/bad") })
    }

    @Test
    fun wrongChecksumEverywhereIsChecksumError() {
        behaviors["/bad"] = Behavior(dataB, corrupt = true)
        try {
            downloader().run(job(file("b.json", dataB, UrlSpec(url("/bad")))), staging(), Events(), Downloader.Control())
            fail()
        } catch (e: DownloadException) {
            assertEquals("checksum", e.kind)
            assertEquals("b.json", e.extras["file"])
        }
        assertFalse(File(staging(), "b.json").exists())
        assertFalse(File(staging(), "b.json.part").exists())
    }

    @Test
    fun notFoundOnFirstMirrorUsesSecond() {
        behaviors["/b"] = Behavior(dataB)
        downloader().run(job(file("b.json", dataB, UrlSpec(url("/missing")), UrlSpec(url("/b")))), staging(), Events(), Downloader.Control())
        assertArrayEquals(dataB, File(staging(), "b.json").readBytes())
        assertEquals(1, requests.count { it.startsWith("/missing") }) // no pointless retries on 404
    }

    @Test
    fun allMirrorsFailingIsNetworkErrorWithStatus() {
        behaviors["/e"] = Behavior(dataB, status = 503)
        try {
            downloader(attempts = 3).run(job(file("b.json", dataB, UrlSpec(url("/e")), UrlSpec(url("/nope")))), staging(), Events(), Downloader.Control())
            fail()
        } catch (e: DownloadException) {
            assertEquals("network", e.kind)
            assertEquals(404, e.extras["status"])
        }
        assertEquals(3, requests.count { it.startsWith("/e") }) // 5xx retried with backoff
    }

    @Test
    fun pauseOnSlowNetworkThenResumeInNewProcessUsesIfRange() {
        behaviors["/a"] = Behavior(dataA, bytesPerSecond = 4_000_000)
        val control = Downloader.Control()
        val ev = object : Downloader.Listener {
            override fun onProgress(received: Long, total: Long, bytesPerSecond: Double) {
                if (received > 1_000_000) control.requestStop(cancel = false)
            }
            override fun onWaiting(reason: String, received: Long, total: Long) {}
            override fun onVerifying() {}
        }
        try {
            downloader().run(job(file("a.onnx", dataA, UrlSpec(url("/a")))), staging(), ev, control)
            fail("should stop")
        } catch (e: StoppedException) {
            assertFalse(e.cancel)
        }
        val part = File(staging(), "a.onnx.part").length()
        assertTrue("part=$part", part in 1_000_000 until dataA.size)
        // "App killed": a brand-new downloader continues from the .part.
        behaviors["/a"]!!.bytesPerSecond = 0
        requests.clear()
        val d2 = downloader()
        assertEquals(part, d2.bytesOnDisk(job(file("a.onnx", dataA, UrlSpec(url("/a")))), staging()))
        d2.run(job(file("a.onnx", dataA, UrlSpec(url("/a")))), staging(), Events(), Downloader.Control())
        assertArrayEquals(dataA, File(staging(), "a.onnx").readBytes())
        assertEquals("/a bytes=$part-", requests.single().substringBeforeLast(' ').substringBeforeLast(' '))
    }

    @Test
    fun changedFileOnServerRestartsViaIfRange() {
        behaviors["/a"] = Behavior(dataA, dropAfter = 800_000)
        val ev = Events()
        // First run: dropped, then fails (1 attempt) leaving a partial with etag v1.
        try {
            downloader(attempts = 1).run(job(file("a.onnx", dataA, UrlSpec(url("/a")))), staging(), ev, Downloader.Control())
        } catch (_: DownloadException) {}
        assertTrue(File(staging(), "a.onnx.part").exists())
        // Server now has a new ETag: If-Range fails → full 200 → restart.
        behaviors["/a"]!!.etag = "\"v2\""
        downloader().run(job(file("a.onnx", dataA, UrlSpec(url("/a")))), staging(), ev, Downloader.Control())
        assertArrayEquals(dataA, File(staging(), "a.onnx").readBytes())
    }

    @Test
    fun notEnoughSpaceUpFront() {
        behaviors["/a"] = Behavior(dataA)
        try {
            downloader(free = { 1_000_000 }).run(job(file("a.onnx", dataA, UrlSpec(url("/a")))), staging(), Events(), Downloader.Control())
            fail()
        } catch (e: DownloadException) {
            assertEquals("storage", e.kind)
            assertTrue((e.extras["needed"] as Long) > dataA.size)
            assertEquals(1_000_000L, e.extras["available"])
        }
        assertTrue(requests.isEmpty())
    }

    @Test
    fun diskFillingUpMidDownload() {
        val big = Random(3).nextBytes(20 * 1024 * 1024)
        behaviors["/big"] = Behavior(big)
        var calls = 0
        try {
            // Enough at the start, then the disk fills up.
            downloader(free = { if (calls++ == 0) Long.MAX_VALUE / 4 else 1024 }).run(
                job(file("big.onnx", big, UrlSpec(url("/big")))), staging(), Events(), Downloader.Control())
            fail()
        } catch (e: DownloadException) {
            assertEquals("storage", e.kind)
        }
    }

    @Test
    fun waitsWhileOfflineThenContinues() {
        behaviors["/b"] = Behavior(dataB)
        val gate = Gate(online = false)
        val ev = Events()
        val t = Thread { Thread.sleep(150); gate.online = true }
        t.start()
        downloader(gate).run(job(file("b.json", dataB, UrlSpec(url("/b")))), staging(), ev, Downloader.Control())
        t.join()
        assertTrue(ev.waiting.get() >= 1)
        assertArrayEquals(dataB, File(staging(), "b.json").readBytes())
    }

    @Test
    fun cancelWhileWaitingOffline() {
        val gate = Gate(online = false)
        val control = Downloader.Control()
        Thread { Thread.sleep(50); control.requestStop(cancel = true) }.start()
        try {
            downloader(gate).run(job(file("b.json", dataB, UrlSpec(url("/b")))), staging(), Events(), control)
            fail()
        } catch (e: StoppedException) {
            assertTrue(e.cancel)
        }
    }

    @Test
    fun redirectKeepsRangeButDropsHeadersOnOtherHost() {
        behaviors["/real"] = Behavior(dataB)
        behaviors["/r"] = Behavior(dataB, redirectTo = url("/real", host = "localhost"))
        downloader().run(job(file("b.json", dataB, UrlSpec(url("/r"), mapOf("Authorization" to "Bearer secret")))), staging(), Events(), Downloader.Control())
        assertArrayEquals(dataB, File(staging(), "b.json").readBytes())
        val first = requests.first { it.startsWith("/r ") }
        val second = requests.first { it.startsWith("/real ") }
        assertTrue(first, first.contains("Bearer secret"))
        assertFalse(second, second.contains("secret"))
    }

    @Test
    fun sizeMismatchFromServerIsRejected() {
        behaviors["/b"] = Behavior(dataB.copyOf(dataB.size - 10))
        try {
            downloader(attempts = 1).run(job(file("b.json", dataB, UrlSpec(url("/b")))), staging(), Events(), Downloader.Control())
            fail()
        } catch (e: DownloadException) {
            assertEquals("checksum", e.kind)
        }
    }

    @Test
    fun alreadyVerifiedFilesAreSkipped() {
        behaviors["/a"] = Behavior(dataA)
        behaviors["/b"] = Behavior(dataB)
        val j = job(file("a.onnx", dataA, UrlSpec(url("/a"))), file("b.json", dataB, UrlSpec(url("/b"))))
        staging().mkdirs()
        File(staging(), "a.onnx").writeBytes(dataA)
        downloader().run(j, staging(), Events(), Downloader.Control())
        assertTrue(requests.none { it.startsWith("/a") })
        assertArrayEquals(dataB, File(staging(), "b.json").readBytes())
    }

    @Test
    fun specValidationRejectsPathTraversal() {
        val bad = listOf("../x", "a/b", ".hidden", "installed.json", "x.part", "")
        for (n in bad) {
            try {
                job(FileSpec(n, 1, sha(byteArrayOf(1)), listOf(UrlSpec(url("/a"))))).validate()
                fail(n)
            } catch (_: IllegalArgumentException) {}
        }
        try {
            JobSpec("../etc", 1, "", false, listOf(FileSpec("a", 1, sha(byteArrayOf(1)), listOf(UrlSpec(url("/a")))))).validate()
            fail()
        } catch (_: IllegalArgumentException) {}
        try {
            job(FileSpec("a", 1, sha(byteArrayOf(1)), listOf(UrlSpec("file:///etc/passwd")))).validate()
            fail()
        } catch (_: IllegalArgumentException) {}
    }

    @Test
    fun contentRangeParsing() {
        assertEquals(100L, Downloader.parseContentRangeStart("bytes 100-199/200"))
        assertNull(Downloader.parseContentRangeStart(null))
        assertTrue(Downloader.isDiskFull(java.io.IOException("write failed: ENOSPC (No space left on device)")))
    }

    @Test
    fun storeInstallsAtomicallyAndRecovers() {
        val store = ModelStore(File(dir, "models"))
        val j = job(file("a.onnx", dataB, UrlSpec(url("/a"))))
        store.saveJob(j, "downloading")
        assertEquals("downloading", store.loadJob("tts-kk")!!.status)
        File(store.stagingDir("tts-kk"), "a.onnx").writeBytes(dataB)
        store.install(j)
        assertEquals(1, store.installed("tts-kk")!!.version)
        assertFalse(store.stagingDir("tts-kk").exists())
        // Update to v2, simulating a crash after the old dir was moved aside.
        val j2 = j.copy(version = 2)
        File(store.stagingDir("tts-kk"), "a.onnx").apply { parentFile!!.mkdirs(); writeBytes(dataB) }
        store.install(j2)
        assertEquals(2, store.installed("tts-kk")!!.version)
        val target = store.modelDir("tts-kk")
        target.renameTo(File(store.root, ".old-tts-kk"))
        store.recover()
        assertEquals(2, store.installed("tts-kk")!!.version)
        // Truncated file → not installed.
        File(target, "a.onnx").writeBytes(byteArrayOf(1, 2, 3))
        assertNull(store.installed("tts-kk"))
    }
}
