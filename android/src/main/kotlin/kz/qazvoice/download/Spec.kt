package kz.qazvoice.download

/** One download location of a file; [headers] go to this URL's host only. */
data class UrlSpec(val url: String, val headers: Map<String, String> = emptyMap())

/** A zstd-compressed copy of a file: downloaded instead, then unpacked. */
data class ZstSpec(val size: Long, val sha256: String, val urls: List<UrlSpec>)

data class FileSpec(val name: String, val size: Long, val sha256: String, val urls: List<UrlSpec>, val zst: ZstSpec? = null) {
    /** Bytes that travel over the network for this file. */
    val downloadSize: Long get() = zst?.size ?: size

    /** The compressed file as a download of its own. */
    fun zstFile(): FileSpec? = zst?.let { FileSpec("$name.zst", it.size, it.sha256, it.urls) }
}

/** A model download: what the Dart side sends with `download`. */
data class JobSpec(
    val id: String,
    val version: Int,
    val name: String,
    val wifiOnly: Boolean,
    val files: List<FileSpec>,
) {
    val totalBytes: Long get() = files.sumOf { it.size }

    /** Bytes downloaded in total (compressed sizes where available). */
    val downloadBytes: Long get() = files.sumOf { it.downloadSize }

    /** Throws [IllegalArgumentException] if anything could escape the model directory. */
    fun validate() {
        require(ID.matches(id)) { "bad model id" }
        require(files.isNotEmpty()) { "no files" }
        for (f in files) {
            require(isSafeName(f.name)) { "bad file name ${f.name}" }
            require(f.size in 0..MAX_FILE) { "bad size" }
            require(SHA.matches(f.sha256)) { "bad sha256 for ${f.name}" }
            require(f.urls.isNotEmpty()) { "no urls for ${f.name}" }
            f.zst?.let { z ->
                require(z.size in 0..MAX_FILE && SHA.matches(z.sha256) && z.urls.isNotEmpty()) { "bad zst for ${f.name}" }
            }
            for (u in f.urls + (f.zst?.urls ?: emptyList())) {
                val scheme = u.url.substringBefore(':').lowercase()
                require(scheme == "https" || scheme == "http") { "unsupported url scheme" }
            }
        }
        require(files.map { it.name }.toSet().size == files.size) { "duplicate file names" }
        require(files.size <= 64) { "too many files" }
    }

    companion object {
        /** Largest file a model may have (4 GiB); 64 files at most, so the total cannot overflow. */
        const val MAX_FILE = 4L shl 30
        val ID = Regex("^[a-z0-9][a-z0-9-]{0,40}$")

        /** Throws unless [id] is a safe model id (it becomes a directory name). */
        fun checkId(id: String?): String {
            require(id != null && ID.matches(id)) { "bad model id" }
            return id
        }
        val SHA = Regex("^[0-9a-f]{64}$")
        private val NAME = Regex("^[A-Za-z0-9_-][A-Za-z0-9._-]{0,100}$")
        fun isSafeName(n: String) = NAME.matches(n) && !n.contains("..") &&
            n != "installed.json" && !n.endsWith(".part") && !n.endsWith(".meta") && n != "job.json"

        @Suppress("UNCHECKED_CAST")
        private fun urls(v: Any?): List<UrlSpec> = (v as List<Map<*, *>>).map { u ->
            UrlSpec(u["url"] as String, ((u["headers"] as? Map<*, *>) ?: emptyMap<String, String>())
                .entries.associate { (k, v) -> k.toString() to v.toString() })
        }

        @Suppress("UNCHECKED_CAST")
        fun fromMap(m: Map<*, *>): JobSpec = JobSpec(
            id = m["id"] as String,
            version = (m["version"] as Number).toInt(),
            name = (m["name"] as? String) ?: (m["id"] as String),
            wifiOnly = m["wifiOnly"] == true,
            files = (m["files"] as List<Map<*, *>>).map { f ->
                FileSpec(
                    name = f["name"] as String,
                    size = (f["size"] as Number).toLong(),
                    sha256 = (f["sha256"] as String).lowercase(),
                    urls = urls(f["urls"]),
                    zst = (f["zst"] as? Map<*, *>)?.let { z ->
                        ZstSpec((z["size"] as Number).toLong(), (z["sha256"] as String).lowercase(), urls(z["urls"]))
                    },
                )
            },
        )
    }
}

/** Why a download failed, in the shape the Dart side maps to typed errors. */
open class DownloadException(
    val kind: String,
    message: String,
    val extras: Map<String, Any?> = emptyMap(),
    cause: Throwable? = null,
) : Exception(message, cause) {
    fun toMap(): Map<String, Any?> = mapOf("kind" to kind, "message" to (message ?: kind)) + extras
}

class StoppedException(val cancel: Boolean) : DownloadException("cancelled", if (cancel) "cancelled" else "paused")
