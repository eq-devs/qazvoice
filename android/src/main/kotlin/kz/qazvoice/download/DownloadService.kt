package kz.qazvoice.download

import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper

/**
 * Foreground service (type `dataSync`) that keeps model downloads running
 * when the app is in the background or swiped away. The work itself is done
 * by [DownloadManager]; this only holds the process in the foreground, shows
 * the progress notification and handles its Pause / Resume / Cancel buttons.
 */
class DownloadService : Service() {
    companion object {
        const val ACTION_PAUSE = "kz.qazvoice.action.PAUSE"
        const val ACTION_RESUME = "kz.qazvoice.action.RESUME"
        const val ACTION_CANCEL = "kz.qazvoice.action.CANCEL"
        const val EXTRA_ID = "id"

        @Volatile private var instance: DownloadService? = null
        private val main = Handler(Looper.getMainLooper())

        fun stopIfRunning() {
            main.post {
                val s = instance ?: return@post
                if (!DownloadManager.isBusy) s.shutdown()
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        DownloadManager.init(applicationContext)
        instance = this
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Must be in the foreground within seconds of startForegroundService().
        goForeground()
        val id = intent?.getStringExtra(EXTRA_ID)
        when (intent?.action) {
            ACTION_PAUSE -> id?.let(DownloadManager::pause)
            ACTION_RESUME -> id?.let(DownloadManager::resume)
            ACTION_CANCEL -> id?.let(DownloadManager::cancel)
        }
        if (!DownloadManager.isBusy) shutdown()
        return START_STICKY
    }

    private fun goForeground() {
        val n = DownloadManager.notifier.ongoing()
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(Notifier.ONGOING_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
            } else {
                startForeground(Notifier.ONGOING_ID, n)
            }
        } catch (e: Exception) {
            // Not allowed now (background start, dataSync time limit…): the
            // download continues while the app process lives.
            DownloadManager.log("startForeground failed: ${e.javaClass.simpleName}")
        }
    }

    /** Android 15+: the daily dataSync budget is used up. */
    override fun onTimeout(startId: Int, fgsType: Int) {
        DownloadManager.log("foreground time limit reached")
        shutdown()
    }

    @Suppress("DEPRECATION")
    private fun shutdown() {
        if (Build.VERSION.SDK_INT >= 24) stopForeground(STOP_FOREGROUND_DETACH) else stopForeground(false)
        if (!DownloadManager.isBusy) DownloadManager.notifier.clearOngoing()
        stopSelf()
    }

    override fun onDestroy() {
        if (instance === this) instance = null
        super.onDestroy()
    }
}
