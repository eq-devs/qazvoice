package kz.qazvoice

import android.Manifest
import android.app.Activity
import android.app.ActivityManager
import android.content.ComponentCallbacks2
import android.content.Context
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.StatFs
import io.flutter.embedding.engine.plugins.FlutterPlugin
import io.flutter.embedding.engine.plugins.activity.ActivityAware
import io.flutter.embedding.engine.plugins.activity.ActivityPluginBinding
import io.flutter.plugin.common.EventChannel
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.PluginRegistry
import kz.qazvoice.audio.AudioDecoder
import kz.qazvoice.audio.M4aEncoder
import kz.qazvoice.audio.Mic
import kz.qazvoice.audio.PcmPlayer
import kz.qazvoice.download.DownloadManager
import kz.qazvoice.download.JobSpec
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger

/** Android side of qazvoice: downloads, audio I/O, decoding, device facts. */
class QazvoicePlugin : FlutterPlugin, MethodChannel.MethodCallHandler, ActivityAware,
    PluginRegistry.RequestPermissionsResultListener {

    private lateinit var ctx: Context
    private lateinit var channel: MethodChannel
    private lateinit var events: EventChannel
    private lateinit var micChannel: EventChannel
    private val main = Handler(Looper.getMainLooper())
    private var eventSink: EventChannel.EventSink? = null
    private var activity: ActivityPluginBinding? = null
    private val io = Executors.newCachedThreadPool { r -> Thread(r, "qazvoice-io").apply { isDaemon = true } }
    private val players = ConcurrentHashMap<Int, PcmPlayer>()
    private val nextPlayer = AtomicInteger(1)
    private val decodeCancelled = ConcurrentHashMap<Int, Boolean>()
    private var mic: Mic? = null
    private val pendingPermissions = HashMap<Int, (Boolean) -> Unit>()
    private var nextRequest = 0x5100

    private val downloadListener: (Map<String, Any?>) -> Unit = { e -> send(e) }

    private val memory = object : ComponentCallbacks2 {
        override fun onTrimMemory(level: Int) {
            // UI_HIDDEN fires on every trip to the background: not memory pressure.
            if (level == ComponentCallbacks2.TRIM_MEMORY_UI_HIDDEN) return
            @Suppress("DEPRECATION")
            if (level >= ComponentCallbacks2.TRIM_MEMORY_RUNNING_LOW) send(mapOf("type" to "trimMemory", "level" to level))
        }

        override fun onConfigurationChanged(newConfig: Configuration) {}

        @Deprecated("Deprecated in Java")
        override fun onLowMemory() = send(mapOf("type" to "trimMemory", "level" to 80))
    }

    override fun onAttachedToEngine(binding: FlutterPlugin.FlutterPluginBinding) {
        ctx = binding.applicationContext
        channel = MethodChannel(binding.binaryMessenger, "kz.qazvoice/main")
        channel.setMethodCallHandler(this)
        events = EventChannel(binding.binaryMessenger, "kz.qazvoice/events")
        events.setStreamHandler(object : EventChannel.StreamHandler {
            override fun onListen(arguments: Any?, sink: EventChannel.EventSink) {
                eventSink = sink
            }

            override fun onCancel(arguments: Any?) {
                eventSink = null
            }
        })
        micChannel = EventChannel(binding.binaryMessenger, "kz.qazvoice/mic")
        micChannel.setStreamHandler(MicHandler())
        ctx.registerComponentCallbacks(memory)
    }

    override fun onDetachedFromEngine(binding: FlutterPlugin.FlutterPluginBinding) {
        channel.setMethodCallHandler(null)
        events.setStreamHandler(null)
        micChannel.setStreamHandler(null)
        ctx.unregisterComponentCallbacks(memory)
        DownloadManager.removeListener(downloadListener)
        players.values.forEach { it.stop() }
        players.clear()
        mic?.stop()
        mic = null
        // Downloads keep running in the service.
    }

    private fun send(e: Map<String, Any?>) {
        main.post { eventSink?.success(e) }
    }

    private fun bg(result: MethodChannel.Result, block: () -> Any?) {
        io.execute {
            try {
                val v = block()
                main.post { result.success(v) }
            } catch (e: kz.qazvoice.download.DownloadException) {
                main.post { result.error(e.kind, e.message, e.extras) }
            } catch (e: IllegalArgumentException) {
                main.post { result.error("argument", e.message, null) }
            } catch (e: AudioDecoder.CancelledException) {
                main.post { result.error("cancelled", "cancelled", null) }
            } catch (e: Exception) {
                main.post { result.error("audio", "${e.javaClass.simpleName}: ${e.message}", null) }
            }
        }
    }

    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "init" -> bg(result) {
                DownloadManager.init(ctx)
                @Suppress("UNCHECKED_CAST")
                (call.argument<Map<String, Any?>>("notification"))?.let { n ->
                    DownloadManager.notifier.texts = n.entries.associate { (k, v) -> k to v.toString() }
                }
                DownloadManager.removeListener(downloadListener)
                DownloadManager.addListener(downloadListener)
                mapOf("modelsDir" to DownloadManager.store.root.path, "states" to DownloadManager.states())
            }
            "device" -> bg(result) { device() }
            "modelState" -> bg(result) { DownloadManager.state(call.argument<String>("id")!!) }
            "download" -> bg(result) {
                val spec = JobSpec.fromMap(call.arguments as Map<*, *>)
                DownloadManager.enqueue(spec)
                null
            }
            "pause" -> bg(result) { DownloadManager.pause(call.argument<String>("id")!!); null }
            "resume" -> bg(result) { DownloadManager.resume(call.argument<String>("id")!!); null }
            "cancel" -> bg(result) { DownloadManager.cancel(call.argument<String>("id")!!); null }
            "delete" -> bg(result) { DownloadManager.delete(call.argument<String>("id")!!); null }
            "storageUsed" -> bg(result) { DownloadManager.usedBytes() }
            "tempDir" -> result.success(ctx.cacheDir.path)
            "requestNotificationPermission" -> {
                if (Build.VERSION.SDK_INT < 33) result.success(true)
                else requestPermission(Manifest.permission.POST_NOTIFICATIONS) { result.success(it) }
            }
            "requestMicrophonePermission" -> {
                if (!declared(Manifest.permission.RECORD_AUDIO)) {
                    result.error("permission", "add <uses-permission android:name=\"android.permission.RECORD_AUDIO\"/> to the app's AndroidManifest.xml",
                        mapOf("permission" to "microphone"))
                } else {
                    requestPermission(Manifest.permission.RECORD_AUDIO) { result.success(it) }
                }
            }
            "playerCreate" -> {
                val rate = call.argument<Int>("rate")!!
                val id = nextPlayer.getAndIncrement()
                try {
                    players[id] = PcmPlayer(ctx, rate, { frames, level ->
                        send(mapOf("type" to "player", "id" to id, "frames" to frames, "level" to level.toDouble()))
                    })
                    result.success(id)
                } catch (e: Exception) {
                    result.error("audio", "cannot open audio output: ${e.message}", null)
                }
            }
            "playerWrite" -> {
                val p = players[call.argument<Int>("id")!!]
                val pcm = call.argument<ByteArray>("pcm")!!
                if (p == null) result.success(null) else p.write(pcm) { main.post { result.success(null) } }
            }
            "playerFinish" -> {
                val id = call.argument<Int>("id")!!
                val p = players[id]
                if (p == null) result.success(null) else p.finish {
                    players.remove(id)
                    main.post { result.success(null) }
                }
            }
            "playerStop" -> {
                players.remove(call.argument<Int>("id")!!)?.stop()
                result.success(null)
            }
            "decode" -> {
                val token = call.argument<Int>("token")!!
                decodeCancelled[token] = false
                bg(result) {
                    try {
                        val r = AudioDecoder.decode(ctx, call.argument<String>("input")!!, File(call.argument<String>("output")!!),
                            { decodeCancelled[token] == true }) { f ->
                            send(mapOf("type" to "decode", "token" to token, "fraction" to f))
                        }
                        mapOf("rate" to r.rate, "channels" to r.channels, "durationMs" to r.durationUs / 1000, "bytes" to r.bytes)
                    } finally {
                        decodeCancelled.remove(token)
                    }
                }
            }
            "cancelDecode" -> {
                val t = call.argument<Int>("token")!!
                if (decodeCancelled.containsKey(t)) decodeCancelled[t] = true
                result.success(null)
            }
            "encodeM4a" -> bg(result) {
                M4aEncoder.encode(File(call.argument<String>("input")!!), call.argument<Int>("rate")!!, File(call.argument<String>("output")!!))
                null
            }
            else -> result.notImplemented()
        }
    }

    private fun device(): Map<String, Any?> {
        val am = ctx.getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager
        val mi = ActivityManager.MemoryInfo().also { am.getMemoryInfo(it) }
        val dir = File(ctx.filesDir, "qazvoice").apply { mkdirs() }
        return mapOf(
            "abis" to Build.SUPPORTED_ABIS.toList(),
            "totalRam" to mi.totalMem,
            "availRam" to mi.availMem,
            "lowRam" to am.isLowRamDevice,
            "freeBytes" to StatFs(dir.path).availableBytes,
            "sdk" to Build.VERSION.SDK_INT,
            "model" to "${Build.MANUFACTURER} ${Build.MODEL}",
        )
    }

    private fun declared(permission: String): Boolean = runCatching {
        val info = ctx.packageManager.getPackageInfo(ctx.packageName, PackageManager.GET_PERMISSIONS)
        info.requestedPermissions?.contains(permission) == true
    }.getOrDefault(true)

    private fun requestPermission(permission: String, done: (Boolean) -> Unit) {
        if (ctx.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED) {
            done(true)
            return
        }
        val act: Activity = activity?.activity ?: run { done(false); return }
        val code = nextRequest++
        pendingPermissions[code] = done
        act.requestPermissions(arrayOf(permission), code)
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray): Boolean {
        val cb = pendingPermissions.remove(requestCode) ?: return false
        cb(grantResults.isNotEmpty() && grantResults[0] == PackageManager.PERMISSION_GRANTED)
        return true
    }

    private inner class MicHandler : EventChannel.StreamHandler {
        override fun onListen(arguments: Any?, sink: EventChannel.EventSink) {
            val rate = ((arguments as? Map<*, *>)?.get("rate") as? Int) ?: 16000
            if (ctx.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                sink.error("permission", "microphone permission not granted", mapOf("permission" to "microphone"))
                sink.endOfStream()
                return
            }
            mic?.stop()
            mic = Mic(rate, { bytes -> main.post { sink.success(bytes) } }, { msg ->
                main.post { sink.error("audio", msg, null) }
            }).also { it.start() }
        }

        override fun onCancel(arguments: Any?) {
            mic?.stop()
            mic = null
        }
    }

    override fun onAttachedToActivity(binding: ActivityPluginBinding) {
        activity = binding
        binding.addRequestPermissionsResultListener(this)
    }

    override fun onDetachedFromActivityForConfigChanges() = onDetachedFromActivity()

    override fun onReattachedToActivityForConfigChanges(binding: ActivityPluginBinding) = onAttachedToActivity(binding)

    override fun onDetachedFromActivity() {
        activity?.removeRequestPermissionsResultListener(this)
        activity = null
        pendingPermissions.values.forEach { it(false) }
        pendingPermissions.clear()
    }
}
