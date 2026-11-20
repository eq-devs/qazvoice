package kz.qazvoice.download

import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Build
import android.os.StatFs
import android.util.Log
import java.io.File
import java.util.concurrent.Executors

/**
 * Process-wide owner of model downloads. Jobs run one at a time on a background
 * thread; their state is persisted in the [ModelStore] so a restarted app (or
 * a restarted service) continues where it stopped. [DownloadService] keeps the
 * process alive and shows the notification while something is downloading.
 */
@SuppressLint("StaticFieldLeak") // holds the application context only
object DownloadManager {
    private const val TAG = "qazvoice"

    private lateinit var ctx: Context
    lateinit var store: ModelStore
        private set
    lateinit var notifier: Notifier
        private set
    private lateinit var downloader: Downloader
    private val gate = AndroidGate()

    private class Job(val spec: JobSpec) {
        val control = Downloader.Control()
        @Volatile var status = "queued" // queued | downloading | paused | failed
        @Volatile var error: Map<String, Any?>? = null
        @Volatile var lastEvent: Map<String, Any?>? = null
    }

    private val lock = Any()
    private val jobs = LinkedHashMap<String, Job>()
    private var running: Job? = null
    private val executor = Executors.newSingleThreadExecutor { r -> Thread(r, "qazvoice-download").apply { isDaemon = true } }
    private val listeners = mutableSetOf<(Map<String, Any?>) -> Unit>()
    @Volatile private var initialized = false

    @Synchronized
    fun init(context: Context) {
        if (initialized) return
        ctx = context.applicationContext
        // noBackupFilesDir: hundreds of MB of models must not go into Auto Backup
        // (they would exceed its quota), nor the mirror headers in job.json.
        val root = File(ctx.noBackupFilesDir, "qazvoice/models")
        migrateFromFilesDir(File(ctx.filesDir, "qazvoice/models"), root)
        store = ModelStore(root).also { it.root.mkdirs(); it.recover() }
        notifier = Notifier(ctx)
        gate.init(ctx)
        downloader = Downloader(gate, { f -> freeBytes(f) })
        initialized = true
        // Re-attach: downloads that were running when the process died continue.
        for (p in store.persistedJobs()) {
            val job = Job(p.spec)
            job.status = if (p.status == "paused" || p.status == "failed") p.status else "queued"
            job.error = p.error
            synchronized(lock) { jobs[p.spec.id] = job }
        }
        if (jobs.values.any { it.status == "queued" }) {
            log("resuming ${jobs.values.filter { it.status == "queued" }.joinToString { it.spec.id }}")
            ensureService()
            pump()
        }
    }

    private fun migrateFromFilesDir(old: File, new: File) {
        if (!old.exists() || new.exists()) return
        new.parentFile?.mkdirs()
        if (!old.renameTo(new)) old.deleteRecursively()
    }

    fun addListener(l: (Map<String, Any?>) -> Unit) = synchronized(listeners) { listeners += l }
    fun removeListener(l: (Map<String, Any?>) -> Unit) = synchronized(listeners) { listeners -= l }

    private fun emit(id: String, m: Map<String, Any?>) {
        val ev = mapOf("type" to "download", "id" to id) + m
        synchronized(lock) { jobs[id]?.lastEvent = m }
        val ls = synchronized(listeners) { listeners.toList() }
        ls.forEach { runCatching { it(ev) } }
    }

    fun log(msg: String) {
        Log.i(TAG, msg)
        val ls = synchronized(listeners) { listeners.toList() }
        ls.forEach { runCatching { it(mapOf("type" to "log", "level" to "info", "message" to msg)) } }
    }

    /** State of one model for the Dart side. */
    fun state(id: String): Map<String, Any?> {
        val installed = store.installed(id)
        val base = mutableMapOf<String, Any?>()
        if (installed != null) {
            base["installedVersion"] = installed.version
            base["installedBytes"] = installed.bytes
        }
        val job = synchronized(lock) { jobs[id] }
        if (job == null) {
            if (installed != null) return base + mapOf("status" to "installed", "version" to installed.version)
            return base + mapOf("status" to "notInstalled", "received" to 0L)
        }
        val total = job.spec.totalBytes
        val received = job.lastEvent?.get("received") as? Long ?: downloader.bytesOnDisk(job.spec, store.stagingDir(id))
        return base + when (job.status) {
            "downloading" -> job.lastEvent ?: mapOf("status" to "downloading", "received" to received, "total" to total)
            "queued" -> mapOf("status" to "waiting", "reason" to "queued", "received" to received, "total" to total)
            "failed" -> mapOf("status" to "failed", "received" to received, "total" to total, "error" to job.error)
            else -> mapOf("status" to "paused", "received" to received, "total" to total)
        }
    }

    fun states(): Map<String, Any?> {
        val ids = mutableSetOf<String>()
        store.root.listFiles()?.forEach { if (it.isDirectory && !it.name.startsWith(".")) ids += it.name }
        synchronized(lock) { ids += jobs.keys }
        return ids.associateWith { state(it) }
    }

    fun enqueue(spec: JobSpec) {
        spec.validate()
        synchronized(lock) {
            val existing = jobs[spec.id]
            if (existing != null && (existing.status == "queued" || existing.status == "downloading")) return
            // A new version replaces the staged bytes of an older one.
            val prev = store.loadJob(spec.id)
            if (prev != null && prev.spec.version != spec.version) store.clearStaging(spec.id)
            val job = Job(spec)
            jobs[spec.id] = job
            store.saveJob(spec, "queued")
        }
        emitQueued(spec.id)
        ensureService()
        pump()
    }

    private fun emitQueued(id: String) {
        val s = state(id)
        emit(id, s.filterKeys { it != "installedVersion" && it != "installedBytes" })
    }

    fun pause(id: String) {
        val job: Job
        val wasQueued: Boolean
        synchronized(lock) {
            job = jobs[id] ?: return
            wasQueued = job.status == "queued"
            when (job.status) {
                "downloading" -> job.control.requestStop(cancel = false)
                "queued" -> job.status = "paused" // the pump only takes "queued" jobs
            }
        }
        if (wasQueued) {
            store.saveJob(job.spec, "paused")
            emit(id, state(id))
            notifier.paused(job.spec, received(job))
        }
    }

    fun resume(id: String) {
        val job = synchronized(lock) { jobs[id] }
        val spec = job?.spec ?: store.loadJob(id)?.spec ?: return
        if (job != null && (job.status == "queued" || job.status == "downloading")) return
        synchronized(lock) { jobs.remove(id) }
        notifier.clear(spec.id)
        enqueue(spec)
    }

    fun cancel(id: String) {
        val job = synchronized(lock) { jobs[id] } ?: run {
            store.clearStaging(id)
            return
        }
        if (job.status == "downloading") {
            job.control.requestStop(cancel = true)
        } else {
            finishCancel(job)
        }
    }

    private fun finishCancel(job: Job) {
        synchronized(lock) { if (jobs[job.spec.id] === job) jobs.remove(job.spec.id) }
        store.clearStaging(job.spec.id)
        notifier.clear(job.spec.id)
        val installed = store.installed(job.spec.id)
        emit(job.spec.id, mapOf("status" to "notInstalled", "received" to 0L, "keepInstalled" to (installed != null)))
        stopServiceIfIdle()
    }

    fun delete(id: String) {
        val job = synchronized(lock) { jobs.remove(id) }
        job?.control?.requestStop(cancel = true)
        notifier.clear(id)
        // Deleting while a download thread writes is fine: it fails and is dropped.
        store.delete(id)
        emit(id, mapOf("status" to "notInstalled", "received" to 0L))
        stopServiceIfIdle()
    }

    fun usedBytes(): Long = store.usedBytes()

    fun freeBytes(f: File): Long = runCatching {
        var d: File? = f
        while (d != null && !d.exists()) d = d.parentFile
        StatFs((d ?: ctx.filesDir).path).availableBytes
    }.getOrDefault(-1)

    val isBusy: Boolean get() = synchronized(lock) { jobs.values.any { it.status == "queued" || it.status == "downloading" } }

    private fun received(job: Job): Long = (job.lastEvent?.get("received") as? Long)
        ?: downloader.bytesOnDisk(job.spec, store.stagingDir(job.spec.id))

    private fun pump() {
        executor.execute {
            while (true) {
                val job = synchronized(lock) {
                    if (running != null) return@execute
                    jobs.values.firstOrNull { it.status == "queued" }?.also { running = it; it.status = "downloading" }
                } ?: break
                try {
                    runJob(job)
                } finally {
                    synchronized(lock) { running = null }
                }
            }
            stopServiceIfIdle()
        }
    }

    private fun runJob(job: Job) {
        val spec = job.spec
        val staging = store.stagingDir(spec.id)
        store.saveJob(spec, "downloading")
        // Others still queued are told so.
        synchronized(lock) { jobs.values.filter { it !== job && it.status == "queued" }.map { it.spec.id } }.forEach { emitQueued(it) }
        log("${spec.id}: downloading v${spec.version} (${spec.totalBytes} bytes)")
        val listener = object : Downloader.Listener {
            override fun onProgress(received: Long, total: Long, bytesPerSecond: Double) {
                emit(spec.id, mapOf("status" to "downloading", "received" to received, "total" to total, "speed" to bytesPerSecond))
                notifier.progress(spec, received, total, bytesPerSecond)
            }

            override fun onWaiting(reason: String, received: Long, total: Long) {
                emit(spec.id, mapOf("status" to "waiting", "reason" to reason, "received" to received, "total" to total))
                notifier.waiting(spec, reason, received, total)
            }

            override fun onVerifying() {
                emit(spec.id, mapOf("status" to "verifying", "received" to spec.totalBytes, "total" to spec.totalBytes))
            }

            override fun log(message: String) = this@DownloadManager.log("${spec.id}: $message")
        }
        try {
            gate.current = job.control
            downloader.run(spec, staging, listener, job.control)
            // A delete/cancel that landed after the last check must win.
            synchronized(lock) {
                if (jobs[spec.id] !== job || job.control.stop) throw StoppedException(cancel = job.control.cancel || jobs[spec.id] !== job)
                store.install(spec)
                jobs.remove(spec.id)
            }
            val bytes = store.installed(spec.id)?.bytes ?: spec.totalBytes
            log("${spec.id}: installed v${spec.version}")
            emit(spec.id, mapOf("status" to "installed", "version" to spec.version, "installedBytes" to bytes))
            notifier.done(spec)
        } catch (e: StoppedException) {
            if (e.cancel || synchronized(lock) { jobs[spec.id] !== job }) {
                if (synchronized(lock) { jobs[spec.id] === job }) finishCancel(job)
            } else {
                job.status = "paused"
                store.saveJob(spec, "paused")
                log("${spec.id}: paused")
                emit(spec.id, state(spec.id))
                notifier.paused(spec, received(job))
            }
        } catch (e: Exception) {
            val err = (e as? DownloadException)?.toMap() ?: mapOf("kind" to "network", "message" to "${e.javaClass.simpleName}: ${e.message}")
            if (synchronized(lock) { jobs[spec.id] !== job }) return // deleted meanwhile
            job.status = "failed"
            job.error = err
            store.saveJob(spec, "failed", err)
            log("${spec.id}: failed: ${err["message"]}")
            emit(spec.id, mapOf("status" to "failed", "received" to received(job), "total" to spec.totalBytes, "error" to err))
            notifier.failed(spec, err["message"]?.toString() ?: "")
        } finally {
            gate.current = null
        }
    }

    private fun ensureService() {
        val i = Intent(ctx, DownloadService::class.java)
        try {
            if (Build.VERSION.SDK_INT >= 26) ctx.startForegroundService(i) else ctx.startService(i)
        } catch (e: Exception) {
            // Background-start restriction: downloads still run while the app lives.
            log("foreground service not started: ${e.javaClass.simpleName}")
        }
    }

    private fun stopServiceIfIdle() {
        if (!isBusy) DownloadService.stopIfRunning()
    }

    /** Network availability from ConnectivityManager; disconnects the running
     *  download as soon as the network drops so it waits instead of timing out. */
    private class AndroidGate : Downloader.NetworkGate {
        private lateinit var cm: ConnectivityManager
        private val signal = Object()
        @Volatile var current: Downloader.Control? = null

        fun init(ctx: Context) {
            cm = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
            runCatching {
                cm.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
                    override fun onAvailable(network: Network) = wake()
                    override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) = wake()
                    override fun onLost(network: Network) {
                        runCatching { current?.connection?.disconnect() }
                        wake()
                    }
                })
            }
        }

        private fun wake() = synchronized(signal) { signal.notifyAll() }

        private fun caps(): NetworkCapabilities? = runCatching { cm.getNetworkCapabilities(cm.activeNetwork) }.getOrNull()

        override fun allowed(wifiOnly: Boolean): Boolean {
            val c = caps() ?: return false
            if (!c.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)) return false
            return !wifiOnly || c.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
        }

        override fun reason(wifiOnly: Boolean): String =
            if (wifiOnly && caps()?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) == true) "wifi" else "network"

        override fun await(wifiOnly: Boolean, control: Downloader.Control): Boolean {
            while (!control.stop) {
                if (allowed(wifiOnly)) return true
                synchronized(signal) { signal.wait(2000) }
            }
            return false
        }
    }
}
