package me.vorotnikov.nsa5gfix

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle

/** UI-less launcher trampoline (nsg-unlock style): tapping the icon starts
 *  the root-side nsa5gfix-diagd child and the tray service; the notification IS
 *  the app. Idempotent — DaemonHost refuses a second start while one child
 *  lives, so a redundant tap is a no-op.
 *
 *  Notification permission is load-bearing: without it the FGS card never
 *  renders and the card is the ONLY Stop handle. On denial the tap routes
 *  to the app's notification settings instead of launching. */
class LaunchActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
            return // launch() runs in onRequestPermissionsResult
        }
        launch()
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        val granted = Build.VERSION.SDK_INT < 33 ||
            grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED
        if (granted) {
            launch()
        } else {
            // No card ⇒ no Stop handle ⇒ no session: send the user to the
            // toggle instead of arming something unmanageable.
            startActivity(
                Intent(android.provider.Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                    .putExtra(android.provider.Settings.EXTRA_APP_PACKAGE, packageName),
            )
            finish()
        }
    }

    private fun launch() {
        DaemonHost.start(this)
        startForegroundService(Intent(this, DaemonService::class.java))
        finish()
    }
}
