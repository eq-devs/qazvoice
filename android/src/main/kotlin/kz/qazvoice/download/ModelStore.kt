package kz.qazvoice.download

import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/**
 * Model files on disk:
 *
 * ```
 * <root>/<id>/<files…>            installed model
 * <root>/<id>/installed.json      written last: its presence = installed
 * <root>/.staging/<id>/<name>.part, <name>.meta, <name>   download in progress
 * <root>/.staging/<id>/job.json   the download spec and its status
 * ```
 */
class ModelStore(val root: File) {
    data class InstalledInfo(val version: Int, val bytes: Long)
    data class PersistedJob(val spec: JobSpec, val status: String, val error: Map<String, Any?>?)

    fun modelDir(id: String) = File(root, id)
    fun stagingDir(id: String) = File(File(root, ".staging"), id)

    fun installed(id: String): InstalledInfo? = runCatching {
        val f = File(modelDir(id), "installed.json")
        if (!f.exists()) return null
        val o = JSONObject(f.readText())
        val files = o.getJSONArray("files")
        var bytes = 0L
        for (i in 0 until files.length()) {
            val e = files.getJSONObject(i)
            val file = File(modelDir(id), e.getString("name"))
            // A file removed or truncated behind our back: not installed.
            if (file.length() != e.getLong("size")) return null
            bytes += e.getLong("size")
        }
        InstalledInfo(o.getInt("version"), bytes)
    }.getOrNull()

    /** Moves the verified files of [job] from staging into place. */
    @Synchronized
    fun install(job: JobSpec) {
        val staging = stagingDir(job.id)
        val target = modelDir(job.id)
        val incoming = File(root, ".incoming-${job.id}")
        val old = File(root, ".old-${job.id}")
        incoming.deleteRecursively()
        old.deleteRecursively()
        incoming.mkdirs()
        for (f in job.files) {
            val src = File(staging, f.name)
            if (src.length() != f.size || !src.renameTo(File(incoming, f.name))) {
                throw DownloadException("checksum", "cannot install ${f.name}", mapOf("file" to f.name))
            }
        }
        if (target.exists() && !target.renameTo(old)) {
            throw DownloadException("storage", "cannot replace ${job.id}")
        }
        if (!incoming.renameTo(target)) {
            old.renameTo(target)
            throw DownloadException("storage", "cannot install ${job.id}")
        }
        val info = JSONObject()
            .put("id", job.id)
            .put("version", job.version)
            .put("installedAt", System.currentTimeMillis())
            .put("files", JSONArray().apply {
                job.files.forEach { put(JSONObject().put("name", it.name).put("size", it.size).put("sha256", it.sha256)) }
            })
        val tmp = File(target, "installed.json.tmp")
        tmp.writeText(info.toString())
        if (!tmp.renameTo(File(target, "installed.json"))) throw DownloadException("storage", "cannot write installed.json")
        old.deleteRecursively()
        staging.deleteRecursively()
    }

    /** Undoes a half-finished [install] after a crash. */
    @Synchronized
    fun recover() {
        root.listFiles()?.forEach { f ->
            when {
                f.name.startsWith(".old-") -> {
                    val id = f.name.removePrefix(".old-")
                    val target = modelDir(id)
                    if (!File(target, "installed.json").exists()) {
                        target.deleteRecursively()
                        f.renameTo(target)
                    } else {
                        f.deleteRecursively()
                    }
                }
                f.name.startsWith(".incoming-") -> f.deleteRecursively()
            }
        }
    }

    @Synchronized
    fun delete(id: String) {
        modelDir(id).deleteRecursively()
        stagingDir(id).deleteRecursively()
    }

    @Synchronized
    fun clearStaging(id: String) {
        stagingDir(id).deleteRecursively()
    }

    @Synchronized
    fun saveJob(spec: JobSpec, status: String, error: Map<String, Any?>? = null) {
        val dir = stagingDir(spec.id)
        dir.mkdirs()
        val o = JSONObject().put("spec", specToJson(spec)).put("status", status)
        if (error != null) o.put("error", JSONObject(error.mapValues { it.value ?: JSONObject.NULL }))
        val tmp = File(dir, "job.json.tmp")
        tmp.writeText(o.toString())
        tmp.renameTo(File(dir, "job.json"))
    }

    fun loadJob(id: String): PersistedJob? = runCatching {
        val f = File(stagingDir(id), "job.json")
        if (!f.exists()) return null
        val o = JSONObject(f.readText())
        val spec = specFromJson(o.getJSONObject("spec"))
        spec.validate()
        val err = o.optJSONObject("error")?.let { e -> e.keys().asSequence().associateWith { k -> e.opt(k).takeUnless { it == JSONObject.NULL } } }
        PersistedJob(spec, o.optString("status", "paused"), err)
    }.getOrNull()

    fun persistedJobs(): List<PersistedJob> =
        File(root, ".staging").listFiles()?.mapNotNull { if (it.isDirectory) loadJob(it.name) else null } ?: emptyList()

    fun usedBytes(): Long = root.walkTopDown().filter { it.isFile }.sumOf { it.length() }

    companion object {
        fun specToJson(s: JobSpec): JSONObject = JSONObject()
            .put("id", s.id).put("version", s.version).put("name", s.name).put("wifiOnly", s.wifiOnly)
            .put("files", JSONArray().apply {
                s.files.forEach { f ->
                    put(JSONObject().put("name", f.name).put("size", f.size).put("sha256", f.sha256)
                        .put("urls", JSONArray().apply {
                            f.urls.forEach { u -> put(JSONObject().put("url", u.url).put("headers", JSONObject(u.headers))) }
                        }))
                }
            })

        fun specFromJson(o: JSONObject): JobSpec {
            val files = o.getJSONArray("files")
            return JobSpec(
                id = o.getString("id"),
                version = o.getInt("version"),
                name = o.optString("name", o.getString("id")),
                wifiOnly = o.optBoolean("wifiOnly", false),
                files = (0 until files.length()).map { i ->
                    val f = files.getJSONObject(i)
                    val urls = f.getJSONArray("urls")
                    FileSpec(f.getString("name"), f.getLong("size"), f.getString("sha256"),
                        (0 until urls.length()).map { j ->
                            val u = urls.getJSONObject(j)
                            val h = u.optJSONObject("headers")
                            UrlSpec(u.getString("url"), h?.keys()?.asSequence()?.associateWith { h.getString(it) } ?: emptyMap())
                        })
                },
            )
        }
    }
}
