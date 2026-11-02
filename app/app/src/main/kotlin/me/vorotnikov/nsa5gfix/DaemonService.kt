package me.vorotnikov.nsa5gfix

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.graphics.drawable.Icon

/** The tray presence while nsa5gfix-diagd runs (nsg-unlock SessionService
 *  style): a foreground service whose notification IS the app — alive ⇔
 *  daemon session. The card shows the live DIAG-derived NR cell state
 *  (ARFCN · PCI · band · freshness), refreshed from the dev.nsa5gfix.*
 *  properties every few seconds, so it doubles as the on-device payoff
 *  display for the whole patch. Stop runs the verified root-side kill
 *  through DaemonHost; the service stops itself as soon as the child is
 *  gone, so the card never outlives the session. */
class DaemonService : Service(), DaemonHost.Listener {
    private lateinit var notifyManager: NotificationManager
    private val mainHandler = Handler(Looper.getMainLooper())
    private val poll = object : Runnable {
        override fun run() {
            if (DaemonHost.childAlive) {
                notifyManager.notify(NOTIFICATION_ID, buildNotification())
                mainHandler.postDelayed(this, POLL_MS)
            } else {
                stopSelf()
            }
        }
    }

    override fun onBind(intent: Intent?) = null

    override fun onCreate() {
        super.onCreate()
        notifyManager = getSystemService(NotificationManager::class.java)
        notifyManager.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "5G NSA daemon", NotificationManager.IMPORTANCE_LOW)
        )
        DaemonHost.addListener(this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (Build.VERSION.SDK_INT >= 34) {
            startForeground(NOTIFICATION_ID, buildNotification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(NOTIFICATION_ID, buildNotification())
        }
        if (intent?.action == ACTION_STOP) DaemonHost.stop()
        if (!DaemonHost.childAlive) stopSelf()
        mainHandler.removeCallbacks(poll)
        mainHandler.postDelayed(poll, POLL_MS)
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        // Every stop path removes the tray card explicitly: a service
        // stopping while foreground can leave its notification posted on
        // this OEM.
        mainHandler.removeCallbacks(poll)
        stopForeground(STOP_FOREGROUND_REMOVE)
        DaemonHost.removeListener(this)
        super.onDestroy()
    }

    override fun onLog(line: String) {}

    override fun onChild(alive: Boolean, detail: String) {
        if (!alive) mainHandler.post { stopSelf() }
    }

    /** The card: daemon state + the live NR cell line. The data line is
     *  the same source the Frida hook substitutes into CellIdentityNr. */
    private fun buildNotification(): Notification {
        val stop = PendingIntent.getService(
            this, 1,
            Intent(this, DaemonService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val arfcn = DaemonHost.prop("dev.nsa5gfix.arfcn").toIntOrNull() ?: -1
        val pci = DaemonHost.prop("dev.nsa5gfix.pci").toIntOrNull() ?: -1
        val band = DaemonHost.prop("dev.nsa5gfix.band").toIntOrNull() ?: -1
        val ts = DaemonHost.prop("dev.nsa5gfix.ts").toLongOrNull() ?: -1
        val dataLine = when {
            arfcn <= 0 -> "waiting for NR data…"
            else -> {
                val age = if (ts > 0) ((SystemClock.elapsedRealtime() - ts) / 1000).toString() + "s" else "?"
                val bandPart = if (band > 0) "n$band · " else ""
                val pciPart = if (pci in 0..1007) "PCI $pci · " else ""
                "ARFCN $arfcn · ${bandPart}${pciPart}age $age"
            }
        }
        return Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_lock_power_off)
            .setContentTitle("5G NSA daemon")
            .setContentText(dataLine)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .addAction(
                Notification.Action.Builder(
                    Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel),
                    "Stop",
                    stop,
                ).build()
            )
            .build()
    }

    companion object {
        private const val CHANNEL_ID = "nsa5gfix"
        private const val NOTIFICATION_ID = 1
        private const val POLL_MS = 1_000L
        const val ACTION_STOP = "me.vorotnikov.nsa5gfix.action.STOP"
    }
}
